//! Finding the project directory (`docs/mcp.md`, "Finding the project").

use std::path::{Path, PathBuf};

/// The project directory: `explicit` (relative to `cwd`) when given, else
/// the first directory from `cwd` upwards that holds `project.json`, or
/// failing that the first one with exactly one `*.worlds` child holding
/// `project.json`. The error says what was tried and how to fix it.
pub fn find_project(explicit: Option<&Path>, cwd: &Path) -> Result<PathBuf, String> {
    find_project_within(explicit, cwd, None)
}

/// [`find_project`], searching upwards no further than `top` (when given):
/// tests bound the search to a directory they control, so whatever lies
/// above the system's temp directory can't answer for them.
fn find_project_within(
    explicit: Option<&Path>,
    cwd: &Path,
    top: Option<&Path>,
) -> Result<PathBuf, String> {
    let ancestors = || {
        let mut done = false;
        cwd.ancestors().take_while(move |d| {
            if done {
                return false;
            }
            done = top.is_some_and(|t| *d == t);
            true
        })
    };
    if let Some(p) = explicit {
        let dir = cwd.join(p);
        if dir.join("project.json").is_file() {
            return canonical(&dir);
        }
        return Err(format!(
            "--project {}: {} has no project.json; pass the project directory (the one holding project.json, e.g. mygame.worlds)",
            p.display(),
            dir.display()
        ));
    }
    let mut tried = Vec::new();
    for dir in ancestors() {
        if dir.join("project.json").is_file() {
            return canonical(dir);
        }
        tried.push(dir.display().to_string());
    }
    for dir in ancestors() {
        let children = worlds_children(dir);
        match children.len() {
            0 => {}
            1 => return canonical(&children[0]),
            _ => {
                let names: Vec<String> = children
                    .iter()
                    .filter_map(|c| c.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                return Err(format!(
                    "{} holds several projects ({}); pick one with --project <dir>",
                    dir.display(),
                    names.join(", ")
                ));
            }
        }
    }
    Err(format!(
        "no Worlds project found: none of {} (or a single *.worlds child of one) holds project.json; start worlds-mcp inside the project or pass --project <dir>",
        tried.join(", ")
    ))
}

/// Child directories named `*.worlds` that hold `project.json`, sorted.
fn worlds_children(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "worlds") && p.join("project.json").is_file()
        })
        .collect();
    out.sort();
    out
}

fn canonical(dir: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_and_finds_a_single_worlds_child() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let proj = repo.join("game.worlds");
        std::fs::create_dir_all(proj.join("worlds")).unwrap();
        std::fs::write(proj.join("project.json"), "{}").unwrap();
        let deep = repo.join("src/x");
        std::fs::create_dir_all(&deep).unwrap();
        let canon = std::fs::canonicalize(&proj).unwrap();
        // Inside the project.
        let top = Some(tmp.path());
        assert_eq!(
            find_project_within(None, &proj.join("worlds"), top).unwrap(),
            canon
        );
        // In the repo, below it: the single *.worlds child.
        assert_eq!(find_project_within(None, &deep, top).unwrap(), canon);
        assert_eq!(
            find_project(Some(Path::new("game.worlds")), &repo).unwrap(),
            canon
        );
        let e = find_project(Some(Path::new("src")), &repo).unwrap_err();
        assert!(e.contains("no project.json"), "{e}");
        // Two projects: ambiguous.
        let other = repo.join("b.worlds");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("project.json"), "{}").unwrap();
        let e = find_project_within(None, &deep, top).unwrap_err();
        assert!(e.contains("several projects"), "{e}");
    }

    #[test]
    fn nothing_found_names_what_was_tried() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("a/b");
        std::fs::create_dir_all(&cwd).unwrap();
        // Bounded at the temp dir: projects in directories above it (a
        // TMPDIR inside a game repo) can't answer.
        let e = find_project_within(None, &cwd, Some(tmp.path())).unwrap_err();
        assert!(e.contains("--project"), "{e}");
        assert!(e.contains(&tmp.path().display().to_string()), "{e}");
        assert!(e.contains(&cwd.display().to_string()), "{e}");
    }

    /// The bound stops the search: a project just above it isn't found.
    #[test]
    fn the_search_stops_at_its_top() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("project.json"), "{}").unwrap();
        let inner = tmp.path().join("inner");
        let cwd = inner.join("x");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(find_project_within(None, &cwd, Some(&inner)).is_err());
        assert!(find_project_within(None, &cwd, Some(tmp.path())).is_ok());
        assert!(find_project(None, &cwd).is_ok());
    }
}
