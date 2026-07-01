use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use pon_aot::BuildOptions;

const USAGE: &str = "usage: pon build <file> -o <out> [--allow-dynamic] [--opt] [--target <triple>]";

pub fn run_from_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    let parsed = parse_args(args)?;
    build_file(&parsed.entry_path, &parsed.options)
}

fn build_file(entry_path: &Path, options: &BuildOptions) -> Result<()> {
    pon_aot::build(entry_path, options).map(|_| ())
}

#[derive(Debug)]
struct ParsedBuild {
    entry_path: PathBuf,
    options: BuildOptions,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<ParsedBuild> {
    let mut args = args.into_iter();
    let entry_path = args
        .next()
        .context("missing file for `pon build <file> -o <out>`")?;

    let mut out_path = None;
    let mut allow_dynamic = false;
    let mut opt = false;
    let mut target = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" => {
                if out_path.is_some() {
                    bail!("duplicate `-o` for `pon build`\n{USAGE}");
                }
                out_path = Some(PathBuf::from(
                    args.next().context("missing output path after `-o`")?,
                ));
            }
            "--allow-dynamic" => allow_dynamic = true,
            "--opt" => opt = true,
            "--target" => {
                if target.is_some() {
                    bail!("duplicate `--target` for `pon build`\n{USAGE}");
                }
                let raw = args.next().context("missing target triple after `--target`")?;
                target = Some(
                    raw.parse()
                        .map_err(|error| anyhow!("invalid target triple `{raw}`: {error}"))?,
                );
            }
            _ if arg.starts_with('-') => bail!("unknown `pon build` option `{arg}`\n{USAGE}"),
            _ => bail!("unexpected argument `{arg}` for `pon build`\n{USAGE}"),
        }
    }

    let out_path = out_path.context("missing `-o <out>` for `pon build`")?;

    Ok(ParsedBuild {
        entry_path: PathBuf::from(entry_path),
        options: BuildOptions {
            out_path,
            allow_dynamic,
            opt,
            target,
        },
    })
}
