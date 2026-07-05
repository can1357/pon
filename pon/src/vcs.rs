//! Git-backed VCS requirement fetching for pon.

use std::{
	fs,
	path::{Component, Path, PathBuf},
	process::{Command, Output},
};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// A parsed git VCS requirement.
///
/// The `clone_url` has the pip-style `git+` prefix, optional `@rev`, and URL
/// fragment removed. `requested_rev` is the revision embedded in the URL, if
/// any, and `subdirectory` is a validated relative path from the repository
/// root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitReference {
	/// URL passed to `git clone`, for example `https://example.test/repo.git`.
	pub clone_url:     String,
	/// Optional revision requested after the repository URL's final path `@`.
	pub requested_rev: Option<String>,
	/// Optional relative package directory from `#subdirectory=...`.
	pub subdirectory:  Option<PathBuf>,
}

/// A concrete git checkout selected for installation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCheckout {
	/// Directory containing the project to install. When the requirement uses
	/// `#subdirectory=...`, this points at that subdirectory rather than the
	/// repository root.
	pub dir:    PathBuf,
	/// Resolved `HEAD` commit id after checkout.
	pub commit: String,
}

/// Parse a pip-style git VCS URL.
///
/// Accepted inputs are `git+https://...`, `git+ssh://...`, and `git+file://...`,
/// each with an optional path revision suffix (`@rev`) and optional
/// `#subdirectory=relative/path` fragment. Stripped clone URLs using the same
/// underlying schemes are also accepted so callers that already removed the
/// `git+` prefix can reuse the parser. Other VCS prefixes (`hg+`, `svn+`, and
/// `bzr+`) are rejected with pon's standard unsupported-VCS requirement error.
pub fn parse_git_reference(raw: &str) -> Result<GitReference> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Err(Error::InvalidRequirement(raw.to_owned()));
	}

	reject_unsupported_vcs(raw)?;

	let body = raw.strip_prefix("git+").unwrap_or(raw);
	let (without_fragment, fragment) = split_fragment(body);
	let subdirectory = parse_subdirectory_fragment(raw, fragment)?;
	let (clone_url, requested_rev) = split_revision(raw, without_fragment)?;

	validate_clone_scheme(raw, clone_url)?;

	Ok(GitReference {
		clone_url: clone_url.to_owned(),
		requested_rev: requested_rev.map(str::to_owned),
		subdirectory,
	})
}

/// Fetch a git repository into pon's VCS cache and return the selected
/// checkout.
///
/// The checkout root is `<cache_root>/git/<sha256(clone-url)>`. A missing cache
/// entry is cloned with `git clone --filter=blob:none`; an existing entry is
/// updated with `git fetch --tags`. The selected revision is `rev` when
/// supplied, otherwise any `@rev` embedded in `url`, otherwise the remote
/// default branch. The returned [`GitCheckout::dir`] is validated after
/// applying an optional `#subdirectory=...` fragment.
pub fn fetch_git(cache_root: &Path, url: &str, rev: Option<&str>) -> Result<GitCheckout> {
	let reference = parse_git_reference(url)?;
	let selected_rev = rev
		.filter(|value| !value.trim().is_empty())
		.map(str::to_owned)
		.or_else(|| reference.requested_rev.clone());

	let repo_dir = cache_root
		.join("git")
		.join(sha256_hex(reference.clone_url.as_bytes()));
	let parent = repo_dir.parent().ok_or_else(|| {
		Error::Cli(format!("could not determine git cache directory for VCS requirement `{url}`"))
	})?;
	fs::create_dir_all(parent)?;

	if repo_dir.exists() {
		if !repo_dir.is_dir() {
			return Err(Error::Cli(format!(
				"git cache path `{}` for VCS requirement `{url}` is not a directory",
				repo_dir.display()
			)));
		}
		run_git(
			git_command()
				.arg("-C")
				.arg(&repo_dir)
				.arg("fetch")
				.arg("--tags"),
			url,
		)?;
	} else {
		run_git(
			git_command()
				.arg("clone")
				.arg("--filter=blob:none")
				.arg(&reference.clone_url)
				.arg(&repo_dir),
			url,
		)?;
	}

	match selected_rev.as_deref() {
		Some(revision) => {
			run_git(
				git_command()
					.arg("-C")
					.arg(&repo_dir)
					.arg("checkout")
					.arg(revision),
				url,
			)?;
		},
		None => {
			run_git(
				git_command()
					.arg("-C")
					.arg(&repo_dir)
					.arg("checkout")
					.arg("--detach")
					.arg("origin/HEAD"),
				url,
			)?;
		},
	}

	let output = run_git(
		git_command()
			.arg("-C")
			.arg(&repo_dir)
			.arg("rev-parse")
			.arg("HEAD"),
		url,
	)?;
	let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
	if commit.is_empty() {
		return Err(Error::Cli(format!("git did not report a commit for VCS requirement `{url}`")));
	}

	let dir = match &reference.subdirectory {
		Some(subdirectory) => {
			let dir = repo_dir.join(subdirectory);
			if !dir.is_dir() {
				return Err(Error::Cli(format!(
					"VCS requirement `{url}` subdirectory `{}` does not exist",
					subdirectory.display()
				)));
			}
			dir
		},
		None => repo_dir,
	};

	Ok(GitCheckout { dir, commit })
}

fn git_command() -> Command {
	Command::new("git")
}

fn run_git(command: &mut Command, requirement_url: &str) -> Result<Output> {
	let output = command.output().map_err(|error| {
		if error.kind() == std::io::ErrorKind::NotFound {
			Error::Cli(format!(
				"git is required for VCS requirement `{requirement_url}` but was not found on PATH"
			))
		} else {
			Error::from(error)
		}
	})?;

	if output.status.success() {
		Ok(output)
	} else {
		let tail = stderr_tail(&output.stderr);
		Err(Error::Cli(format!(
			"git command failed for VCS requirement `{requirement_url}` ({}): {tail}",
			output.status
		)))
	}
}

fn reject_unsupported_vcs(raw: &str) -> Result<()> {
	let Some((scheme, _rest)) = raw.split_once('+') else {
		return Ok(());
	};

	if matches!(scheme, "hg" | "svn" | "bzr") {
		return Err(Error::InvalidRequirement(format!(
			"unsupported VCS scheme `{scheme}`; only git is supported"
		)));
	}

	Ok(())
}

fn split_fragment(raw: &str) -> (&str, Option<&str>) {
	raw.split_once('#')
		.map_or((raw, None), |(url, fragment)| (url, Some(fragment)))
}

fn parse_subdirectory_fragment(raw: &str, fragment: Option<&str>) -> Result<Option<PathBuf>> {
	let Some(fragment) = fragment else {
		return Ok(None);
	};

	for item in fragment.split('&') {
		let Some((key, value)) = item.split_once('=') else {
			continue;
		};
		if key == "subdirectory" {
			return validate_subdirectory(raw, value).map(Some);
		}
	}

	Ok(None)
}

fn validate_subdirectory(raw: &str, value: &str) -> Result<PathBuf> {
	if value.is_empty() {
		return Err(Error::InvalidRequirement(raw.to_owned()));
	}

	let path = PathBuf::from(value);
	if path.is_absolute() {
		return Err(Error::InvalidRequirement(raw.to_owned()));
	}

	let mut normalized = PathBuf::new();
	for component in path.components() {
		match component {
			Component::Normal(part) => normalized.push(part),
			Component::CurDir => {},
			Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
				return Err(Error::InvalidRequirement(raw.to_owned()));
			},
		}
	}

	if normalized.as_os_str().is_empty() {
		return Err(Error::InvalidRequirement(raw.to_owned()));
	}

	Ok(normalized)
}

fn split_revision<'a>(raw: &str, url: &'a str) -> Result<(&'a str, Option<&'a str>)> {
	let Some(index) = revision_separator_index(url) else {
		return Ok((url, None));
	};

	let clone_url = &url[..index];
	let revision = &url[index + 1..];
	if clone_url.is_empty() || revision.is_empty() {
		return Err(Error::InvalidRequirement(raw.to_owned()));
	}

	Ok((clone_url, Some(revision)))
}

fn revision_separator_index(url: &str) -> Option<usize> {
	let scheme_end = url.find("://")? + 3;
	let path_start = url[scheme_end..]
		.find('/')
		.map(|offset| scheme_end + offset)?;
	let candidate = url.rfind('@')?;

	(candidate > path_start).then_some(candidate)
}

fn validate_clone_scheme(raw: &str, clone_url: &str) -> Result<()> {
	if clone_url.starts_with("https://")
		|| clone_url.starts_with("ssh://")
		|| clone_url.starts_with("file://")
	{
		return Ok(());
	}

	Err(Error::InvalidRequirement(raw.to_owned()))
}

fn sha256_hex(bytes: &[u8]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let digest = Sha256::digest(bytes);
	let mut out = String::with_capacity(digest.len() * 2);
	for byte in digest {
		out.push(HEX[(byte >> 4) as usize] as char);
		out.push(HEX[(byte & 0x0f) as usize] as char);
	}
	out
}

fn stderr_tail(bytes: &[u8]) -> String {
	let text = String::from_utf8_lossy(bytes);
	let text = text.trim();
	if text.is_empty() {
		return "<no stderr>".to_owned();
	}

	let mut lines = text.lines().rev().take(20).collect::<Vec<_>>();
	lines.reverse();
	let joined = lines.join("\n");
	tail_chars(&joined, 4000)
}

fn tail_chars(text: &str, limit: usize) -> String {
	let count = text.chars().count();
	if count <= limit {
		return text.to_owned();
	}

	let tail = text.chars().skip(count - limit).collect::<String>();
	format!("...{tail}")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_git_https_with_revision_and_subdirectory() {
		let parsed = parse_git_reference(
			"git+https://example.test/org/repo.git@v1.2.3#subdirectory=packages/demo",
		)
		.expect("parsed git reference");

		assert_eq!(parsed.clone_url, "https://example.test/org/repo.git");
		assert_eq!(parsed.requested_rev.as_deref(), Some("v1.2.3"));
		assert_eq!(parsed.subdirectory.as_deref(), Some(Path::new("packages/demo")));
	}

	#[test]
	fn parses_git_ssh_userinfo_without_treating_user_at_as_revision() {
		let parsed = parse_git_reference("git+ssh://git@example.test/org/repo.git@main")
			.expect("parsed git ssh reference");

		assert_eq!(parsed.clone_url, "ssh://git@example.test/org/repo.git");
		assert_eq!(parsed.requested_rev.as_deref(), Some("main"));
	}

	#[test]
	fn parses_git_file_without_revision() {
		let parsed = parse_git_reference("git+file:///tmp/repo#subdirectory=src/pkg")
			.expect("parsed git file reference");

		assert_eq!(parsed.clone_url, "file:///tmp/repo");
		assert_eq!(parsed.requested_rev, None);
		assert_eq!(parsed.subdirectory.as_deref(), Some(Path::new("src/pkg")));
	}

	#[test]
	fn rejects_unsupported_vcs_prefix_with_planned_message() {
		let error = parse_git_reference("hg+https://example.test/repo").expect_err("unsupported");

		assert_eq!(
			error.to_string(),
			"invalid requirement `unsupported VCS scheme `hg`; only git is supported`"
		);
	}

	#[test]
	fn rejects_absolute_or_escaping_subdirectories() {
		assert!(parse_git_reference("git+https://example.test/repo.git#subdirectory=/abs").is_err());
		assert!(
			parse_git_reference("git+https://example.test/repo.git#subdirectory=../pkg").is_err()
		);
	}
}
