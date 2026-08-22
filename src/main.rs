use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use swatch::{BuildSide, PackRoot, TOOL_NAME, authoring, publish};

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
    if command == "init" {
        let options = parse_init_args(&args)?;
        let path = swatch::init::init(&options)?;
        println!("initialized {}", path.display());
        return Ok(());
    }
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        print_help();
        return Ok(());
    }
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
        "build" => {
            let sides = parse_build_sides(&args)?;
            for side in sides {
                println!("{}", swatch::build(&root, side)?.display());
            }
            Ok(())
        }
        "prepare" => {
            if !args.is_empty() {
                return Err(format!("invalid prepare arguments; use `{TOOL_NAME} prepare`").into());
            }
            println!("{}", publish::prepare_release(&root)?.display());
            Ok(())
        }
        "verify" => {
            if !args.is_empty() {
                return Err(format!("invalid verify arguments; use `{TOOL_NAME} verify`").into());
            }
            let release = publish::verify_release(&root)?;
            println!(
                "verified {} prepared artifacts for pack {}",
                release.artifacts.len(),
                release.pack_version
            );
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
            "--resource-pack" => set_placement(
                &mut placement_flag,
                swatch::spec::ContentPlacement::ResourcePack,
                "--resource-pack",
            )?,
            "--datapack" => set_placement(
                &mut placement_flag,
                swatch::spec::ContentPlacement::DataPack,
                "--datapack",
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
            "usage: {TOOL_NAME} add <project> [--version <version>] [--client|--server|--shader|--resource-pack|--datapack]"
        ))
    })?;
    options.placement = placement_flag.map(|(placement, _)| placement);
    Ok((query, version, options))
}

fn parse_build_sides(args: &[String]) -> swatch::Result<Vec<BuildSide>> {
    match args {
        [side] if side == "client" => Ok(vec![BuildSide::Client]),
        [side] if side == "server" => Ok(vec![BuildSide::Server]),
        [side] if side == "all" => Ok(vec![BuildSide::Client, BuildSide::Server]),
        _ => Err(format!(
            "invalid build arguments; use `{TOOL_NAME} build client`, `{TOOL_NAME} build server`, or `{TOOL_NAME} build all`"
        )
        .into()),
    }
}

fn parse_init_args(args: &[String]) -> swatch::Result<swatch::init::InitOptions> {
    let mut path = None;
    let mut name = None;
    let mut slug = None;
    let mut group = "org.example.packs".to_string();
    let mut minecraft = None;
    let mut loader = None;
    let mut loader_version = None;
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        if value.starts_with("--") {
            let (key, inline) = value
                .split_once('=')
                .map_or((value.as_str(), None), |(key, value)| (key, Some(value)));
            let option = match inline {
                Some(value) => value.to_string(),
                None => {
                    index += 1;
                    args.get(index)
                        .ok_or_else(|| format!("{key} requires a value"))?
                        .clone()
                }
            };
            match key {
                "--name" => name = Some(option),
                "--slug" => slug = Some(option),
                "--group" => group = option,
                "--minecraft" => minecraft = Some(option),
                "--loader" => loader = Some(option),
                "--loader-version" => loader_version = Some(option),
                _ => return Err(format!("unknown init option `{key}`").into()),
            }
        } else if path.is_none() {
            path = Some(PathBuf::from(value));
        } else {
            return Err(format!("unexpected init argument `{value}`").into());
        }
        index += 1;
    }
    let path = path.unwrap_or_else(|| PathBuf::from("."));
    let slug = slug.or_else(|| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
    });
    let required = || {
        swatch::Error::from(format!(
            "usage: {TOOL_NAME} init <directory> --name <name> --minecraft <version> --loader <fabric|forge|neoforge> --loader-version <version> [--slug <slug>] [--group <group>]"
        ))
    };
    Ok(swatch::init::InitOptions {
        path,
        name: name.ok_or_else(required)?,
        slug: slug.ok_or_else(required)?,
        group,
        minecraft: minecraft.ok_or_else(required)?,
        loader: loader.ok_or_else(required)?,
        loader_version: loader_version.ok_or_else(required)?,
    })
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
Swatch
Minecraft pack authoring tool

  swatch init <directory>    Create a complete pack repository
  swatch add <project>       Add exact-pinned Modrinth content and install it
  swatch remove <project>    Remove content and install the pack
  swatch install              Resolve, download, and verify the pack
  swatch install --curseforge Refresh CurseForge mappings with Packwiz
  swatch build client        Build the client archive
  swatch build server        Build the dedicated server archive
  swatch build all           Build both archives
  swatch prepare             Prepare artifacts and write dist/release.json
  swatch verify              Verify every prepared artifact without credentials
  swatch publish --dry-run   Prepare and show upload details without uploading
  swatch publish             Publish the verified dist/release.json snapshot
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
        let (_, _, resources) =
            parse_add_args(&args(&["fresh", "--resource-pack"])).expect("resource pack placement");
        assert_eq!(
            resources.placement,
            Some(swatch::spec::ContentPlacement::ResourcePack)
        );
        let (_, _, data) =
            parse_add_args(&args(&["tectonic", "--datapack"])).expect("datapack placement");
        assert_eq!(
            data.placement,
            Some(swatch::spec::ContentPlacement::DataPack)
        );
    }

    #[test]
    fn build_grammar_names_each_side() {
        assert_eq!(
            parse_build_sides(&args(&["client"])).expect("client build"),
            [BuildSide::Client]
        );
        assert_eq!(
            parse_build_sides(&args(&["all"])).expect("all builds"),
            [BuildSide::Client, BuildSide::Server]
        );
        assert!(parse_build_sides(&args(&["universal"])).is_err());
    }

    #[test]
    fn init_uses_the_directory_slug_and_requires_pack_versions() {
        let options = parse_init_args(&args(&[
            "example-pack",
            "--name",
            "Example Pack",
            "--minecraft=26.2",
            "--loader",
            "neoforge",
            "--loader-version",
            "26.2.0",
        ]))
        .expect("init options");
        assert_eq!(options.slug, "example-pack");
        assert_eq!(options.group, "org.example.packs");
        assert!(parse_init_args(&args(&["example-pack", "--name", "Example"])).is_err());
    }
}
