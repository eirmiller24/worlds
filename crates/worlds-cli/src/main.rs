//! `worlds-cli`: headless tools over a Worlds project directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

mod check_export;
mod enum_source;

#[derive(Parser)]
#[command(
    name = "worlds-cli",
    version,
    about = "Headless tools for Worlds projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load a project and report every problem. Exits 1 if there are any.
    Validate {
        /// Project directory.
        dir: PathBuf,
        /// Also compare an enum with the game data file it mirrors:
        /// <enum>=<file.json>[#<json-pointer>]. The values are the object
        /// keys (keys starting with _ skipped) or the array of strings at the
        /// pointer; each value missing on either side is a problem.
        /// Repeatable. The file is relative to the current directory.
        #[arg(long = "enum-source", value_name = "ENUM=FILE[#POINTER]", value_parser = enum_source::EnumSource::parse)]
        enum_sources: Vec<enum_source::EnumSource>,
        /// Values the Worlds enum may have that its --enum-source doesn't:
        /// <enum>=<value>[,<value>...] (e.g. theme=none). Repeatable.
        #[arg(long = "enum-extra", value_name = "ENUM=VALUES", value_parser = enum_source::EnumExtra::parse)]
        enum_extras: Vec<enum_source::EnumExtra>,
    },
    /// Check an export directory against the export format, without the
    /// game. Exits 0 when clean (warnings
    /// allowed), 1 on problems, 2 on a usage or I/O error.
    CheckExport {
        /// Export directory (the one holding world.json). Default:
        /// <project>/export, with --project.
        export: Option<PathBuf>,
        /// Project directory, for --fresh. Default: the export directory's
        /// parent when it holds a project.json.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Also bake the project in memory as if into the export directory
        /// and report every file that differs (a stale export).
        #[arg(long)]
        fresh: bool,
        /// Print { ok, levels, problems, warnings } as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write a JSON Schema per file kind into <dir>/schema/.
    Schema {
        /// Project directory.
        dir: PathBuf,
    },
    /// Create an empty project (one empty world `main`) plus its schemas.
    New {
        /// Directory to create; must not exist or be empty.
        dir: PathBuf,
        /// Project name. Defaults to the directory name without `.worlds`.
        #[arg(long)]
        name: Option<String>,
        /// Cell size in pixels (`grid_size`), e.g. 32 for 32 x 32 px art.
        #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=1024))]
        grid: u32,
    },
    /// Convert an LDtk project (.ldtk) into a Worlds project directory.
    /// Prints every warning and renamed identifier.
    ImportLdtk {
        /// The .ldtk file.
        input: PathBuf,
        /// Project directory to write, e.g. mygame.worlds. Tileset images are
        /// referenced relative to it, not copied.
        #[arg(long)]
        out: PathBuf,
    },
    /// Bake every auto layer and write the game export: <out>/world.json,
    /// <out>/enums.json and <out>/levels/<slug>.json.
    Bake {
        /// Project directory.
        dir: PathBuf,
        /// Only this level (world.json and enums.json are still written).
        #[arg(long)]
        level: Option<String>,
        /// Export directory. Default: <dir>/export.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import definitions, worlds or levels from another Worlds project
    /// (docs/importing_worlds.md). Prints the plan, then applies it as one
    /// batch and saves; --dry-run only prints it.
    ImportProject {
        /// The project to import from.
        source: PathBuf,
        /// The project to import into (written in place).
        #[arg(long)]
        into: PathBuf,
        /// Every definition and project setting, no levels.
        #[arg(long, conflicts_with_all = ["all", "levels", "worlds"])]
        defs_only: bool,
        /// Every definition, setting and world with its levels.
        #[arg(long, conflicts_with_all = ["levels", "worlds"])]
        all: bool,
        /// These levels (comma-separated), with what they need.
        #[arg(long, value_delimiter = ',', value_name = "LEVEL,...")]
        levels: Vec<String>,
        /// These worlds (comma-separated): their levels, guide groups and
        /// world notes.
        #[arg(long, value_delimiter = ',', value_name = "WORLD,...")]
        worlds: Vec<String>,
        /// What to do with a slug this project already has (with another
        /// definition): keep ours, take theirs, or take theirs as <slug>_2.
        #[arg(long, value_enum, default_value_t = OnConflict::Skip)]
        on_conflict: OnConflict,
        /// Put imported levels into this existing world.
        #[arg(long, conflicts_with_all = ["new_world", "keep_worlds"])]
        world: Option<String>,
        /// Put imported levels into a new world with this slug (default: a
        /// new world named after the source project).
        #[arg(long, conflicts_with = "keep_worlds")]
        new_world: Option<String>,
        /// Put each level into the world of its own source world's slug,
        /// made if missing.
        #[arg(long)]
        keep_worlds: bool,
        /// World pixels added to every imported level's position: x,y.
        #[arg(long, value_name = "X,Y", allow_hyphen_values = true)]
        offset: Option<String>,
        /// Print the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print the plan (and what was applied) as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OnConflict {
    Skip,
    Overwrite,
    Rename,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    // Writers of a project (or of its export, baked from disk): warn when a
    // Worlds session that crashed left unsaved edits in its autosave.
    if let Command::Bake { dir, .. } = &cli.command {
        worlds_core::autosave::warn_pending(dir);
    }
    if let Command::ImportProject {
        into,
        dry_run: false,
        ..
    } = &cli.command
    {
        worlds_core::autosave::warn_pending(into);
    }
    match cli.command {
        Command::Validate {
            dir,
            enum_sources,
            enum_extras,
        } => validate(&dir, &enum_sources, &enum_extras),
        Command::CheckExport {
            export,
            project,
            fresh,
            json,
        } => check_export(export, project, fresh, json),
        Command::Schema { dir } => {
            worlds_core::schema::write_all(&dir)?;
            println!("wrote {}", dir.join("schema").display());
            Ok(ExitCode::SUCCESS)
        }
        Command::New { dir, name, grid } => new(&dir, name, grid),
        Command::ImportLdtk { input, out } => import_ldtk(&input, &out),
        Command::Bake { dir, level, out } => bake(&dir, level.as_deref(), out.as_deref()),
        Command::ImportProject {
            source,
            into,
            defs_only,
            all,
            levels,
            worlds,
            on_conflict,
            world,
            new_world,
            keep_worlds,
            offset,
            dry_run,
            json,
        } => {
            use worlds_core::merge::{
                Conflict, Placement, PlacementMode, Policy, Preset, Selection,
            };
            let slugs = |v: Vec<String>, what: &str| -> Result<Vec<worlds_core::Slug>> {
                v.into_iter()
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        worlds_core::Slug::new(s.clone())
                            .map_err(|_| anyhow::anyhow!("{what} {s:?} is not a slug"))
                    })
                    .collect()
            };
            let mut selection = Selection {
                levels: slugs(levels, "level")?,
                worlds: slugs(worlds, "world")?,
                ..Selection::default()
            };
            selection.preset = if all {
                Preset::Everything
            } else if defs_only {
                Preset::DefsOnly
            } else if selection.levels.is_empty() && selection.worlds.is_empty() {
                bail!("say what to import: --defs-only, --all, --levels or --worlds");
            } else {
                Preset::Pick
            };
            let offset = match offset {
                None => [0, 0],
                Some(s) => {
                    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
                    match parts.as_slice() {
                        [x, y] => [
                            x.parse().context("--offset x")?,
                            y.parse().context("--offset y")?,
                        ],
                        _ => bail!("--offset is x,y in world pixels, e.g. 640,0"),
                    }
                }
            };
            let slug = |s: String| {
                worlds_core::Slug::new(s.clone())
                    .map_err(|_| anyhow::anyhow!("world {s:?} is not a slug"))
            };
            let placement = match (world, new_world, keep_worlds) {
                (Some(w), _, _) => Placement {
                    mode: PlacementMode::World,
                    world: Some(slug(w)?),
                    offset,
                },
                (None, _, true) => Placement {
                    mode: PlacementMode::Keep,
                    world: None,
                    offset,
                },
                (None, w, false) => Placement {
                    mode: PlacementMode::NewWorld,
                    world: w.map(slug).transpose()?,
                    offset,
                },
            };
            let policy = Policy {
                on_conflict: match on_conflict {
                    OnConflict::Skip => Conflict::Skip,
                    OnConflict::Overwrite => Conflict::Overwrite,
                    OnConflict::Rename => Conflict::Rename,
                },
                conflicts: Default::default(),
                placement,
            };
            import_project(&source, &into, &selection, &policy, dry_run, json)
        }
    }
}

fn import_project(
    source: &Path,
    into: &Path,
    selection: &worlds_core::merge::Selection,
    policy: &worlds_core::merge::Policy,
    dry_run: bool,
    json: bool,
) -> Result<ExitCode> {
    let src = worlds_core::load(source).with_context(|| source.display().to_string())?;
    let mut dst = worlds_core::load(into).with_context(|| into.display().to_string())?;
    let plan = worlds_core::merge::plan(&src, &dst, selection, policy)?;
    let mut copied = Vec::new();
    if !dry_run && !plan.is_empty() {
        copied = worlds_core::merge::copy_assets(&plan, source, into)
            .context("copying files into the project")?;
        dst.apply(plan.command())?;
        worlds_core::save_dirty(&mut dst, into)?;
    }
    if json {
        let mut v = serde_json::to_value(&plan)?;
        v["applied"] = (!dry_run && !plan.is_empty()).into();
        v["copied"] = serde_json::to_value(&copied)?;
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{}", plan.text());
        if dry_run {
            println!("dry run: nothing changed");
        } else if plan.is_empty() {
            println!("nothing to import");
        } else {
            println!(
                "imported into {}: {} ({} file(s) copied)",
                into.display(),
                plan.summary,
                copied.len()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn check_export(
    export: Option<PathBuf>,
    project: Option<PathBuf>,
    fresh: bool,
    json: bool,
) -> Result<ExitCode> {
    let export = match (export, &project) {
        (Some(e), _) => e,
        (None, Some(p)) => p.join("export"),
        (None, None) => bail!("check-export needs an export directory or --project"),
    };
    if project.is_some() && !fresh {
        bail!("--project is only used with --fresh");
    }
    let mut report = check_export::check(&export)?;
    if fresh {
        let project = match project {
            Some(p) => p,
            None => match export.parent() {
                Some(p) if p.join("project.json").is_file() => p.to_path_buf(),
                _ => bail!(
                    "--fresh: {} has no project around it; pass --project",
                    export.display()
                ),
            },
        };
        check_export::fresh(&project, &export, &mut report)?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    } else {
        for p in &report.problems {
            println!("{p}");
        }
        for w in &report.warnings {
            println!("warning: {w}");
        }
        let warned = match report.warnings.len() {
            0 => String::new(),
            n => format!(", {n} warning(s)"),
        };
        if report.problems.is_empty() {
            println!("ok: {} levels, no problems{warned}", report.levels);
        } else {
            println!("{} problem(s){warned}", report.problems.len());
        }
    }
    Ok(if report.problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn validate(
    dir: &Path,
    sources: &[enum_source::EnumSource],
    extras: &[enum_source::EnumExtra],
) -> Result<ExitCode> {
    let project = worlds_core::load(dir)?;
    let mut problems = worlds_core::validate(&project);
    problems.extend(enum_source::drift(&project, sources, extras)?);
    for p in &problems {
        println!("{p}");
    }
    if problems.is_empty() {
        println!("ok: {} levels, no problems", project.levels().count());
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{} problem(s)", problems.len());
        Ok(ExitCode::from(1))
    }
}

fn import_ldtk(input: &Path, out: &Path) -> Result<ExitCode> {
    let imported = worlds_core::ldtk::import(input, out)?;
    for w in &imported.report.warnings {
        println!("warning: {w}");
    }
    for (from, to) in &imported.report.renamed {
        println!("renamed: {from} -> {to}");
    }
    worlds_core::ldtk::write(&imported, out)?;
    let p = &imported.project;
    println!(
        "imported {} into {}: {} layers, {} levels, {} entity types, {} rules, {} warning(s)",
        input.display(),
        out.display(),
        p.layers.len(),
        p.levels().count(),
        p.entities.len(),
        p.rules
            .iter()
            .flat_map(|r| &r.groups)
            .map(|g| g.rules.len())
            .sum::<usize>(),
        imported.report.warnings.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn bake(dir: &Path, only: Option<&str>, out: Option<&Path>) -> Result<ExitCode> {
    let project = worlds_core::load(dir)?;
    if let Some(slug) = only
        && project.level(slug).is_none()
    {
        bail!("no level {slug:?} in {}", dir.display());
    }
    let images = worlds_rules::TilesetImages::new(dir, &project.tilesets);
    let mut baked = BTreeMap::new();
    let mut tiles = 0;
    for level in project.levels() {
        if only.is_some_and(|s| s != level.slug.as_str()) {
            continue;
        }
        let layers = worlds_rules::bake_for_export(&project, level, &images)
            .with_context(|| format!("level {}", level.slug))?;
        tiles += layers.tiles.values().map(Vec::len).sum::<usize>();
        baked.insert(level.slug.clone(), layers);
    }
    let export = out.map_or_else(|| dir.join("export"), Path::to_path_buf);
    let written =
        worlds_core::export::write_counted(&project, dir, &export, &baked, only.is_none())?;
    let changed = written.changed;
    let removed = match written.removed {
        0 => String::new(),
        n => format!(", {n} removed"),
    };
    println!(
        "baked {} level(s), {tiles} autotiles; {} files ({changed} changed{removed}) in {}",
        baked.len(),
        written.files.len(),
        export.display()
    );
    // What a game importer can't read faithfully (currently none).
    for w in worlds_rules::export_warnings(&project, &baked) {
        eprintln!("warning: {w}");
    }
    Ok(ExitCode::SUCCESS)
}

fn new(dir: &Path, name: Option<String>, grid: u32) -> Result<ExitCode> {
    if dir.exists()
        && dir
            .read_dir()
            .with_context(|| dir.display().to_string())?
            .next()
            .is_some()
    {
        bail!("{} already exists and is not empty", dir.display());
    }
    let name = name.unwrap_or_else(|| {
        let stem = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        stem.strip_suffix(".worlds").unwrap_or(&stem).to_string()
    });
    let mut project = worlds_core::Project::new(name);
    project.grid_size = grid;
    worlds_core::save(&mut project, dir)?;
    worlds_core::schema::write_all(dir)?;
    println!("created {}", dir.display());
    Ok(ExitCode::SUCCESS)
}
