use std::env;
use std::process::ExitCode;
use swatch::{PackRoot, TOOL_NAME, authoring, publish};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> swatch::Result<()> {
    let mut args = args;
    if args.is_empty() {
        print_help();
        return Ok(());
    }
    let command = args.remove(0);
    let root = PackRoot::discover(&env::current_dir()?)?;
    match command.as_str() {
        "add" => {
            let (query, version, options) = parse_add_args(&args)?;
            let project = authoring::add(&root, &query, version.as_deref(), options)?;
            let report = authoring::install(&root, authoring::InstallOptions::default())?;
            eprintln!("added {project} and installed {} files", report.files);
            Ok(())
        }
        "remove" => {
            if args.len() != 1 {
                return Err(format!("usage: {TOOL_NAME} remove <project>").into());
            }
            authoring::remove(&root, &args[0])?;
            let report = authoring::install(&root, authoring::InstallOptions::default())?;
            eprintln!("removed {} and installed {} files", args[0], report.files);
            Ok(())
        }
        "install" => {
            let options = parse_install_options(&args)?;
            let report = authoring::install(&root, options)?;
            eprintln!("installed {} files", report.files);
            Ok(())
        }
        "publish" => {
            let mode = parse_publish_mode(&args)?;
            let uploaded = publish::publish(&root, mode)?;
            for item in uploaded {
                println!("{item}");
            }
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command `{other}`").into()),
    }
}

fn parse_install_options(args: &[String]) -> swatch::Result<authoring::InstallOptions> {
    match args {
        [] => Ok(authoring::InstallOptions::default()),
        [flag] if flag == "--curseforge" => Ok(authoring::InstallOptions { curseforge: true }),
        _ => Err(format!(
            "invalid install arguments; use `{TOOL_NAME} install` or `{TOOL_NAME} install --curseforge`"
        )
        .into()),
    }
}

fn parse_publish_mode(args: &[String]) -> swatch::Result<publish::PublishMode> {
    match args {
        [flag] if flag == "--dry-run" => Ok(publish::PublishMode::DryRun),
        [] => Ok(publish::PublishMode::Publish),
        _ => {
            Err(format!(
                "invalid publish arguments; use `{TOOL_NAME} publish` or `{TOOL_NAME} publish --dry-run`"
            )
            .into())
        }
    }
}

fn parse_add_args(
    args: &[String],
) -> swatch::Result<(String, Option<String>, authoring::AddOptions)> {
    let mut query = None;
    let mut version = None;
    let mut options = authoring::AddOptions::default();
    let mut placement_flag = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--client" => set_placement(
                &mut placement_flag,
                swatch::spec::ContentPlacement::ClientMod,
                "--client",
            )?,
            "--server" => set_placement(
                &mut placement_flag,
                swatch::spec::ContentPlacement::ServerMod,
                "--server",
            )?,
            "--shader" => set_placement(
                &mut placement_flag,
                swatch::spec::ContentPlacement::Shader,
                "--shader",
            )?,
            "--version" => {
                index += 1;
                version = Some(args.get(index).ok_or("--version requires a value")?.clone());
            }
            value if value.starts_with("--version=") => {
                version = Some(value[10..].to_string());
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown add option `{value}`").into());
            }
            value if query.is_none() => {
                if let Some((project, requested)) = value.split_once('@') {
                    query = Some(project.to_string());
                    if version.is_none() {
                        version = Some(requested.to_string());
                    }
                } else {
                    query = Some(value.to_string());
                }
            }
            value => return Err(format!("unexpected add argument `{value}`").into()),
        }
        index += 1;
    }
    let query = query.ok_or_else(|| {
        swatch::Error::from(format!(
            "usage: {TOOL_NAME} add <project> [--version <version>] [--client|--server|--shader]"
        ))
    })?;
    options.placement = placement_flag.map(|(placement, _)| placement);
    Ok((query, version, options))
}

fn set_placement(
    selected: &mut Option<(swatch::spec::ContentPlacement, &'static str)>,
    placement: swatch::spec::ContentPlacement,
    flag: &'static str,
) -> swatch::Result<()> {
    if let Some((_, previous)) = selected {
        return Err(format!("add placement flags conflict: {previous} and {flag}").into());
    }
    *selected = Some((placement, flag));
    Ok(())
}

fn print_help() {
    eprintln!(
        "\
Swatch — Minecraft pack authoring tool

  swatch add <project>       Add a Modrinth mod or shader and install it
  swatch remove <project>    Remove a mod or shader and install the pack
  swatch install              Resolve, download, and verify the pack
  swatch install --curseforge Refresh CurseForge mappings with Packwiz
  swatch publish --dry-run    Show release upload details without uploading
  swatch publish               Publish the prepared release to configured targets
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn publishing_accepts_dry_run_or_publish() {
        assert_eq!(
            parse_publish_mode(&args(&["--dry-run"])).expect("dry run"),
            publish::PublishMode::DryRun
        );
        assert_eq!(
            parse_publish_mode(&[]).expect("publish"),
            publish::PublishMode::Publish
        );
        assert!(parse_publish_mode(&args(&["--dry-rnu"])).is_err());
    }

    #[test]
    fn install_only_uses_packwiz_when_requested() {
        assert!(!parse_install_options(&[]).expect("install").curseforge);
        assert!(
            parse_install_options(&args(&["--curseforge"]))
                .expect("CurseForge mappings")
                .curseforge
        );
        assert!(parse_install_options(&args(&["--unknown"])).is_err());
    }

    #[test]
    fn add_rejects_conflicting_placement_flags() {
        assert!(parse_add_args(&args(&["sodium", "--client", "--server"])).is_err());
        assert!(parse_add_args(&args(&["iris", "--shader", "--client"])).is_err());
        let (_, _, options) =
            parse_add_args(&args(&["dedicated", "--server"])).expect("server placement");
        assert_eq!(
            options.placement,
            Some(swatch::spec::ContentPlacement::ServerMod)
        );
    }
}
