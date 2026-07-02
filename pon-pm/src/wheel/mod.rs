pub mod compat;
pub mod filename;
pub(crate) mod record;
pub(crate) mod scripts;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use zip::ZipArchive;
use zip::result::ZipError;

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::install::{
    InstallReport, InstalledPackageRecord, ResolvedRecord, direct_url::DirectUrl, upsert_installed_package,
};

use self::compat::{
    WheelCompatibility, classify_archive_member, classify_root_is_purelib, classify_tags, default_supported_tags,
};
use self::filename::WheelFilename;
use self::record::{RecordEntry, parse_record, write_record};
use self::scripts::{EntryPoint, parse_entry_points};

struct WheelInspection {
    record_path: PathBuf,
    record_text: String,
    top_level_text: Option<String>,
    entry_points_text: Option<String>,
}

pub fn install_wheel(
    env: &EnvLayout,
    resolved_record: &ResolvedRecord,
    wheel_path: &Path,
    direct_url: Option<&DirectUrl>,
) -> Result<InstallReport> {
    let filename = wheel_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::InvalidWheelFilename(wheel_path.display().to_string()))?;
    let wheel = validate_compatible_wheel(filename)?;
    let normalized_record_name = resolved_record.normalized_name();
    if wheel.normalized_distribution != normalized_record_name {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` distribution `{}` does not match resolved package `{}`",
            wheel.normalized_distribution, normalized_record_name
        )));
    }

    let mut archive = open_archive(wheel_path, filename)?;
    let inspection = inspect_archive(filename, &mut archive)?;
    let parsed_record_entries = parse_record(&inspection.record_text, &inspection.record_path.display().to_string())?;
    let dist_info_dir = dist_info_dir(&inspection.record_path, filename)?;
    let record_path = dist_info_dir.join("RECORD");

    env.create_dirs()?;
    let mut installed_record_entries = extract_archive(
        env,
        filename,
        &wheel,
        &mut archive,
        &parsed_record_entries,
        &record_path,
    )?;

    let import_names = import_names(&inspection.top_level_text, &installed_record_entries);

    installed_record_entries.push(write_recorded_file(
        &env.site_packages.join(&dist_info_dir).join("INSTALLER"),
        path_to_record_string(&dist_info_dir.join("INSTALLER"))?,
        b"pon-pm\n",
        false,
    )?);

    if let Some(direct_url) = direct_url {
        let mut direct_url_json = direct_url.to_json()?;
        if !direct_url_json.ends_with('\n') {
            direct_url_json.push('\n');
        }
        installed_record_entries.push(write_recorded_file(
            &env.site_packages.join(&dist_info_dir).join("direct_url.json"),
            path_to_record_string(&dist_info_dir.join("direct_url.json"))?,
            direct_url_json.as_bytes(),
            false,
        )?);
    }

    if let Some(entry_points_text) = &inspection.entry_points_text {
        let entry_points = parse_entry_points(entry_points_text, &format!("{filename} entry_points.txt"))?;
        for entry_point in entry_points
            .console_scripts
            .iter()
            .chain(entry_points.gui_scripts.iter())
        {
            let script_entry = write_entry_point_script(
                env,
                filename,
                &normalized_record_name,
                &resolved_record.version,
                entry_point,
            )?;
            installed_record_entries.retain(|entry| entry.path != script_entry.path);
            installed_record_entries.push(script_entry);
        }
    }

    installed_record_entries.push(RecordEntry {
        path: path_to_record_string(&record_path)?,
        hash: None,
        size: None,
    });
    let record_text = write_record(&installed_record_entries);
    write_file(&env.site_packages.join(&record_path), record_text.as_bytes())?;

    upsert_installed_package(
        env,
        InstalledPackageRecord {
            name: normalized_record_name.clone(),
            version: resolved_record.version.clone(),
            artifact_kind: "wheel".to_owned(),
            import_names: import_names.clone(),
            record_path: Some(record_path),
        },
    )?;

    Ok(InstallReport {
        package_name: normalized_record_name,
        version: resolved_record.version.clone(),
        artifact_kind: "wheel".to_owned(),
        import_names,
    })
}

pub fn validate_compatible_wheel(filename: &str) -> Result<WheelFilename> {
    let wheel = WheelFilename::parse(filename)?;
    match classify_tags(&wheel.tags(), &default_supported_tags()) {
        WheelCompatibility::PurePython => Ok(wheel),
        WheelCompatibility::CAbiRefused { reason } => Err(cabi_refused(filename, &reason)),
    }
}

fn open_archive(path: &Path, label: &str) -> Result<ZipArchive<File>> {
    let file = File::open(path)?;
    ZipArchive::new(file).map_err(|error| zip_error(label, error))
}

fn inspect_archive(filename: &str, archive: &mut ZipArchive<File>) -> Result<WheelInspection> {
    let mut wheel_metadata = None;
    let mut record_path = None;
    let mut record_text = None;
    let mut top_level_text = None;
    let mut entry_points_text = None;

    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(filename, error))?;
        let member_name = member.name().to_owned();
        match classify_archive_member(&member_name) {
            WheelCompatibility::PurePython => {}
            WheelCompatibility::CAbiRefused { reason } => return Err(cabi_refused(filename, &reason)),
        }

        if member_name.ends_with(".dist-info/WHEEL") {
            let mut metadata = String::new();
            member.read_to_string(&mut metadata)?;
            wheel_metadata = Some(metadata);
        } else if member_name.ends_with(".dist-info/RECORD") {
            let mut text = String::new();
            member.read_to_string(&mut text)?;
            record_path = Some(safe_site_relative_path(&member_name)?);
            record_text = Some(text);
        } else if member_name.ends_with(".dist-info/top_level.txt") {
            let mut text = String::new();
            member.read_to_string(&mut text)?;
            top_level_text = Some(text);
        } else if member_name.ends_with(".dist-info/entry_points.txt") {
            let mut text = String::new();
            member.read_to_string(&mut text)?;
            entry_points_text = Some(text);
        }
    }

    let metadata = wheel_metadata.ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "wheel `{filename}` does not contain a .dist-info/WHEEL member"
        ))
    })?;
    match classify_root_is_purelib(&metadata) {
        WheelCompatibility::PurePython => {}
        WheelCompatibility::CAbiRefused { reason } => return Err(cabi_refused(filename, &reason)),
    }

    Ok(WheelInspection {
        record_path: record_path.ok_or_else(|| {
            Error::UnsupportedArtifact(format!(
                "wheel `{filename}` does not contain a .dist-info/RECORD member"
            ))
        })?,
        record_text: record_text.expect("record path and text are set together"),
        top_level_text,
        entry_points_text,
    })
}

fn extract_archive(
    env: &EnvLayout,
    filename: &str,
    wheel: &WheelFilename,
    archive: &mut ZipArchive<File>,
    record_entries: &[RecordEntry],
    record_path: &Path,
) -> Result<Vec<RecordEntry>> {
    let record_by_path = record_entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let dist_info_dir = dist_info_dir(record_path, filename)?;
    let generated_installer = dist_info_dir.join("INSTALLER");
    let generated_direct_url = dist_info_dir.join("direct_url.json");
    let mut installed_entries = Vec::new();

    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(filename, error))?;
        let member_name = member.name().to_owned();
        let relative_path = safe_site_relative_path(&member_name)?;
        let destination = archive_destination(env, filename, wheel, &member_name, &relative_path)?;

        if member.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }

        let Some(record_entry) = record_by_path.get(member_name.as_str()).copied() else {
            return Err(Error::UnsupportedArtifact(format!(
                "wheel `{filename}` member `{member_name}` is missing from RECORD"
            )));
        };

        if relative_path == record_path {
            continue;
        }

        if relative_path == generated_installer || relative_path == generated_direct_url {
            verify_recorded_member(filename, record_entry, &mut member)?;
            continue;
        }

        if data_scheme(wheel, &member_name) == Some("scripts") {
            installed_entries.push(extract_script_member(
                env,
                filename,
                record_entry,
                &mut member,
                &destination,
            )?);
        } else {
            installed_entries.push(extract_regular_member(
                env,
                filename,
                record_entry,
                &mut member,
                &destination,
            )?);
        }
    }

    Ok(installed_entries)
}

fn extract_regular_member(
    env: &EnvLayout,
    filename: &str,
    record_entry: &RecordEntry,
    reader: &mut impl Read,
    destination: &Path,
) -> Result<RecordEntry> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = File::create(destination)?;
    let mut hasher = Sha256::new();
    let copied = copy_and_hash(reader, &mut out, &mut hasher)?;
    let digest = hasher.finalize();
    if let Err(error) = verify_record_entry(filename, record_entry, copied, &digest) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(record_entry_from_digest(
        record_path_for_destination(env, destination)?,
        copied,
        &digest,
    ))
}

fn extract_script_member(
    env: &EnvLayout,
    filename: &str,
    record_entry: &RecordEntry,
    reader: &mut impl Read,
    destination: &Path,
) -> Result<RecordEntry> {
    let mut original = Vec::new();
    reader.read_to_end(&mut original)?;
    let original_digest = Sha256::digest(&original);
    verify_record_entry(filename, record_entry, original.len() as u64, &original_digest)?;

    let installed = rewrite_python_shebang(original);
    write_file(destination, &installed)?;
    chmod_executable(destination)?;
    Ok(RecordEntry::from_bytes(
        record_path_for_destination(env, destination)?,
        &installed,
    ))
}

fn verify_recorded_member(filename: &str, record_entry: &RecordEntry, reader: &mut impl Read) -> Result<()> {
    let mut sink = std::io::sink();
    let mut hasher = Sha256::new();
    let copied = copy_and_hash(reader, &mut sink, &mut hasher)?;
    let digest = hasher.finalize();
    verify_record_entry(filename, record_entry, copied, &digest)
}

fn archive_destination(
    env: &EnvLayout,
    filename: &str,
    wheel: &WheelFilename,
    member_name: &str,
    relative_path: &Path,
) -> Result<PathBuf> {
    let Some((scheme, rest)) = data_member_parts(wheel, member_name)? else {
        return Ok(env.site_packages.join(relative_path));
    };
    let rest = if rest.is_empty() {
        PathBuf::new()
    } else {
        safe_site_relative_path(rest)?
    };
    match scheme {
        "purelib" | "platlib" => Ok(env.site_packages.join(rest)),
        "scripts" => Ok(env.scripts_dir.join(rest)),
        "data" => Ok(env.pon_dir.join(rest)),
        "headers" => Ok(env.pon_dir.join("include").join(&wheel.normalized_distribution).join(rest)),
        _ => Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` has unsupported data scheme `{scheme}`"
        ))),
    }
}

fn data_scheme<'a>(wheel: &WheelFilename, member_name: &'a str) -> Option<&'a str> {
    data_member_parts(wheel, member_name).ok().flatten().map(|(scheme, _)| scheme)
}

fn data_member_parts<'a>(
    wheel: &WheelFilename,
    member_name: &'a str,
) -> Result<Option<(&'a str, &'a str)>> {
    let data_prefix = format!("{}-{}.data/", wheel.distribution, wheel.version);
    let Some(rest) = member_name.strip_prefix(&data_prefix) else {
        return Ok(None);
    };
    let Some((scheme, rest)) = rest.split_once('/') else {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel member `{member_name}` has a malformed .data path"
        )));
    };
    if scheme.is_empty() {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel member `{member_name}` has a malformed .data path"
        )));
    }
    Ok(Some((scheme, rest)))
}

fn write_entry_point_script(
    env: &EnvLayout,
    filename: &str,
    dist: &str,
    version: &str,
    entry_point: &EntryPoint,
) -> Result<RecordEntry> {
    let script_name = safe_script_name(filename, &entry_point.name)?;
    let target = format!("{}:{}", entry_point.module, entry_point.attr);
    if target.contains('\'') {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` entry point `{}` contains an unsupported quote",
            entry_point.name
        )));
    }
    let content = format!(
        "#!/bin/sh\n# generated by pon-pm; console script for {dist} {version}\nexec \"${{PON_PM:-pon-pm}}\" run --entry '{target}' -- \"$@\"\n"
    );
    let path = env.scripts_dir.join(script_name);
    let record_path = record_path_for_destination(env, &path)?;
    write_recorded_file(&path, record_path, content.as_bytes(), true)
}

fn safe_script_name(filename: &str, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` entry point script name `{name}` is not safe"
        )));
    }
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(path.to_path_buf()),
        _ => Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` entry point script name `{name}` is not safe"
        ))),
    }
}

fn import_names(top_level_text: &Option<String>, record_entries: &[RecordEntry]) -> Vec<String> {
    if let Some(top_level_text) = top_level_text {
        let names = dedupe_preserving_order(
            top_level_text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned),
        );
        if !names.is_empty() {
            return names;
        }
    }
    import_names_from_record(record_entries)
}

fn import_names_from_record(entries: &[RecordEntry]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for entry in entries {
        let path = Path::new(&entry.path);
        let mut components = path.components();
        let Some(Component::Normal(first)) = components.next() else {
            continue;
        };
        let Some(first) = first.to_str() else {
            continue;
        };
        if first.ends_with(".dist-info") || first.ends_with(".data") {
            continue;
        }
        match components.next() {
            Some(Component::Normal(second)) if second.to_str() == Some("__init__.py") => {
                names.insert(first.to_owned());
            }
            None if first.ends_with(".py") => {
                names.insert(first.trim_end_matches(".py").to_owned());
            }
            _ => {}
        }
    }
    names.into_iter().collect()
}

fn dedupe_preserving_order(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for name in names {
        if seen.insert(name.clone()) {
            out.push(name);
        }
    }
    out
}

fn dist_info_dir(record_path: &Path, filename: &str) -> Result<PathBuf> {
    record_path.parent().map(Path::to_path_buf).ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "wheel `{filename}` RECORD path `{}` is not inside a .dist-info directory",
            record_path.display()
        ))
    })
}

fn copy_and_hash(reader: &mut impl Read, writer: &mut impl Write, hasher: &mut Sha256) -> Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut copied = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(copied);
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        copied += read as u64;
    }
}

fn verify_record_entry(
    filename: &str,
    record_entry: &RecordEntry,
    copied: u64,
    digest: impl AsRef<[u8]>,
) -> Result<()> {
    if let Some(size) = record_entry.size
        && copied != size
    {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` failed RECORD size verification: expected {size}, got {copied}",
            record_entry.path
        )));
    }
    let Some(hash) = &record_entry.hash else {
        return Ok(());
    };
    let Some((algorithm, expected)) = hash.split_once('=') else {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` has invalid RECORD hash `{hash}`",
            record_entry.path
        )));
    };
    if algorithm != "sha256" {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` uses unsupported RECORD hash algorithm `{algorithm}`",
            record_entry.path
        )));
    }
    let actual = URL_SAFE_NO_PAD.encode(digest.as_ref());
    if actual != expected {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` failed RECORD hash verification",
            record_entry.path
        )));
    }
    Ok(())
}

fn safe_site_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.contains('\\') {
        return Err(Error::UnsupportedArtifact(format!("unsafe wheel member path `{path}`")));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(Error::UnsupportedArtifact(format!(
            "unsafe wheel member path `{}`",
            path.display()
        )));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::UnsupportedArtifact(format!(
                    "unsafe wheel member path `{}`",
                    path.display()
                )));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Error::UnsupportedArtifact(format!(
            "unsafe wheel member path `{}`",
            path.display()
        )));
    }
    Ok(out)
}

fn record_path_for_destination(env: &EnvLayout, destination: &Path) -> Result<String> {
    if let Ok(relative) = destination.strip_prefix(&env.site_packages) {
        return path_to_record_string(relative);
    }
    if let Ok(relative) = destination.strip_prefix(&env.pon_dir) {
        return path_to_record_string(&Path::new("..").join("..").join(relative));
    }
    Err(Error::UnsupportedArtifact(format!(
        "installed wheel path `{}` is outside the pon environment",
        destination.display()
    )))
}

fn path_to_record_string(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(path_component_to_string(part)?),
            Component::ParentDir => parts.push("..".to_owned()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::UnsupportedArtifact(format!(
                    "RECORD path `{}` is not relative",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(Error::UnsupportedArtifact("RECORD path is empty".to_owned()));
    }
    Ok(parts.join("/"))
}

fn path_component_to_string(component: &std::ffi::OsStr) -> Result<String> {
    component.to_str().map(ToOwned::to_owned).ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "wheel installed path component `{}` is not valid UTF-8",
            component.to_string_lossy()
        ))
    })
}

fn record_entry_from_digest(path: String, size: u64, digest: impl AsRef<[u8]>) -> RecordEntry {
    RecordEntry {
        path,
        hash: Some(format!("sha256={}", URL_SAFE_NO_PAD.encode(digest.as_ref()))),
        size: Some(size),
    }
}

fn write_recorded_file(path: &Path, record_path: String, content: &[u8], executable: bool) -> Result<RecordEntry> {
    write_file(path, content)?;
    if executable {
        chmod_executable(path)?;
    }
    Ok(RecordEntry::from_bytes(record_path, content))
}

fn write_file(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn rewrite_python_shebang(mut bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.starts_with(b"#!python") {
        return bytes;
    }
    let mut rewritten = b"#!/usr/bin/env -S pon-pm run".to_vec();
    if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
        rewritten.extend_from_slice(&bytes[newline..]);
    }
    bytes.clear();
    rewritten
}

fn chmod_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn cabi_refused(filename: &str, reason: &str) -> Error {
    Error::UnsupportedArtifact(format!(
        "wheel `{filename}` requires the CPython C-ABI (ob_refcnt): {reason}; this is a by-design limitation of pon"
    ))
}

fn zip_error(filename: &str, error: ZipError) -> Error {
    Error::UnsupportedArtifact(format!("invalid wheel archive `{filename}`: {error}"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::install::{read_installed_packages, remove_installed_package};

    #[test]
    fn installs_idna_pure_wheel_by_extracting_fixture_and_registry_record() {
        let layout = EnvLayout::new(temp_project("idna-wheel"));
        let wheel_path = fixture_wheel("idna-3.10-py3-none-any.whl");
        let record = ResolvedRecord::wheel("idna", "3.10", "idna-3.10-py3-none-any.whl");

        let report = install_wheel(&layout, &record, &wheel_path, None).expect("install");

        assert_eq!(report.import_names, vec!["idna"]);
        assert!(layout.site_packages.join("idna").join("__init__.py").is_file());
        assert!(layout.site_packages.join("idna-3.10.dist-info").join("RECORD").is_file());
        let registry = read_installed_packages(&layout).expect("registry");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name, "idna");
        assert_eq!(registry[0].import_names, vec!["idna"]);
        assert_eq!(
            registry[0].record_path.as_deref(),
            Some(Path::new("idna-3.10.dist-info/RECORD"))
        );
    }

    #[test]
    fn installs_flit_core_pure_wheel_by_extracting_fixture() {
        let layout = EnvLayout::new(temp_project("flit-core-wheel"));
        let wheel_path = fixture_wheel("flit_core-3.12.0-py3-none-any.whl");
        let record = ResolvedRecord::wheel("flit-core", "3.12.0", "flit_core-3.12.0-py3-none-any.whl");

        install_wheel(&layout, &record, &wheel_path, None).expect("install");

        let marker = fs::read_to_string(layout.site_packages.join("flit_core").join("__init__.py")).expect("marker");
        assert!(marker.contains("__version__ = \"3.12.0\""));
    }

    #[test]
    fn remove_round_trip_deletes_record_files_and_empty_parents() {
        let layout = EnvLayout::new(temp_project("remove-wheel"));
        let wheel_path = fixture_wheel("idna-3.10-py3-none-any.whl");
        let record = ResolvedRecord::wheel("idna", "3.10", "idna-3.10-py3-none-any.whl");
        install_wheel(&layout, &record, &wheel_path, None).expect("install");

        let removed = remove_installed_package(&layout, "IDNA").expect("remove");

        assert!(removed.is_some());
        assert!(!layout.site_packages.join("idna").exists());
        assert!(!layout.site_packages.join("idna-3.10.dist-info").exists());
        assert!(read_installed_packages(&layout).expect("registry").is_empty());
        let remaining = fs::read_dir(&layout.site_packages)
            .expect("site-packages")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("entries");
        assert!(remaining.is_empty());
    }

    #[test]
    fn refuses_c_abi_platform_wheel_with_explicit_reason() {
        let error = validate_compatible_wheel("numpy-2.0.0-cp312-cp312-macosx_14_0_arm64.whl")
            .expect_err("platform wheel should fail");
        let message = error.to_string();
        assert!(message.contains("requires the CPython C-ABI"));
        assert!(message.contains("ob_refcnt"));
    }

    #[test]
    fn refuses_mistagged_wheel_with_native_member() {
        let root = temp_project("native-member");
        fs::create_dir_all(&root).expect("root");
        let wheel_path = root.join("demo-1.0-py3-none-any.whl");
        write_test_wheel(
            &wheel_path,
            [
                ("demo/__init__.py", b"__version__ = '1.0'\n".as_slice()),
                ("demo/_native.so", b"not really a shared library".as_slice()),
                (
                    "demo-1.0.dist-info/WHEEL",
                    b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".as_slice(),
                ),
                ("demo-1.0.dist-info/METADATA", b"Name: demo\nVersion: 1.0\n".as_slice()),
            ],
        );
        let layout = EnvLayout::new(root.join("project"));
        let record = ResolvedRecord::wheel("demo", "1.0", "demo-1.0-py3-none-any.whl");

        let error = install_wheel(&layout, &record, &wheel_path, None).expect_err("native member");

        let message = error.to_string();
        assert!(message.contains("ob_refcnt"));
        assert!(message.contains("native extension member"));
    }

    fn write_test_wheel<const N: usize>(path: &Path, members: [(&str, &[u8]); N]) {
        use zip::CompressionMethod;
        use zip::write::SimpleFileOptions;

        let file = File::create(path).expect("wheel");
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut record_rows = Vec::new();
        for (name, bytes) in members {
            zip.start_file(name, options).expect("member");
            zip.write_all(bytes).expect("body");
            let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(bytes));
            record_rows.push(format!("{name},sha256={digest},{}", bytes.len()));
        }
        record_rows.push("demo-1.0.dist-info/RECORD,,".to_owned());
        zip.start_file("demo-1.0.dist-info/RECORD", options).expect("record");
        zip.write_all(record_rows.join("\n").as_bytes()).expect("record body");
        zip.write_all(b"\n").expect("record newline");
        zip.finish().expect("finish");
    }

    fn fixture_wheel(filename: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("wheels")
            .join(filename)
    }

    fn temp_project(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-wheel-{label}-{unique}"))
    }
}
