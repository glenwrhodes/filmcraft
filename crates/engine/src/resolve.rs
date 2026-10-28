//! DaVinci Resolve projects: finding them in the local Resolve project libraries
//! (`file.listResolveProjects`) and importing one (`file.importResolve`). The database is read
//! by `filmcraft_interchange::resolve`; this module finds it and merges the result like any
//! other interchange import.

use serde_json::{Value, json};

use crate::media_browser::DirEntry;
use crate::{EngineError, Result, Session};

/// Where Resolve keeps its local ("disk") project libraries, relative to the home folder, on
/// Windows, macOS and Linux; older versions called the library "Resolve Disk Database".
const LIBRARY_ROOTS: &[&str] = &[
    "AppData/Roaming/Blackmagic Design/DaVinci Resolve/Support/Resolve Project Library",
    "AppData/Roaming/Blackmagic Design/DaVinci Resolve/Support/Resolve Disk Database",
    "Library/Application Support/Blackmagic Design/DaVinci Resolve/Resolve Project Library",
    "Library/Application Support/Blackmagic Design/DaVinci Resolve/Resolve Disk Database",
    ".local/share/DaVinciResolve/Resolve Project Library",
    ".local/share/DaVinciResolve/Resolve Disk Database",
];

/// Linux installs keep the default library next to the application.
const SYSTEM_ROOTS: &[&str] = &["/opt/resolve/Resolve Project Library", "/opt/resolve/Resolve Disk Database"];

/// The file each project folder holds.
pub const PROJECT_FILE: &str = "Project.db";

/// Project folders nest in Project Manager folders; deeper is a link loop.
const MAX_DEPTH: usize = 8;
/// Directories looked at per scan, so a huge or looping library can't stall the app.
const MAX_DIRS: usize = 5000;

/// One project found in a library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    /// The Resolve user (profile) it belongs to.
    pub user: String,
    /// Project Manager folders above it.
    pub folder: Vec<String>,
    /// Its `Project.db`.
    pub path: String,
    pub modified: Option<u64>,
    pub size: Option<u64>,
}

fn join(dir: &str, name: &str) -> String {
    let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
    if dir.ends_with(['/', '\\']) { format!("{dir}{name}") } else { format!("{dir}{sep}{name}") }
}

/// The libraries to scan: `root` when given, else the standard locations that exist.
fn libraries(s: &Session, root: Option<&str>) -> Vec<String> {
    if let Some(r) = root {
        return vec![r.to_string()];
    }
    let mut out: Vec<String> = s.services.home_dir().map(|h| LIBRARY_ROOTS.iter().map(|r| join(&h, r)).collect()).unwrap_or_default();
    out.extend(SYSTEM_ROOTS.iter().map(|r| r.to_string()));
    out
}

fn entries(s: &Session, dir: &str) -> Vec<DirEntry> {
    match s.services.list_entries(dir) {
        Some(Ok(e)) => e,
        _ => Vec::new(),
    }
}

/// Every project in the libraries, newest first.
pub fn find_projects(s: &Session, root: Option<&str>) -> Vec<Found> {
    let mut out = Vec::new();
    let mut budget = MAX_DIRS;
    for lib in libraries(s, root) {
        // <library>/Resolve Projects/Users/<user>/Projects/…, or a Users or Projects folder given directly
        let users_dir = join(&join(&lib, "Resolve Projects"), "Users");
        let listed = entries(s, &users_dir);
        let users: Vec<(String, String)> = if !listed.is_empty() {
            listed.into_iter().filter(|e| e.is_dir).map(|e| (e.name.clone(), join(&join(&users_dir, &e.name), "Projects"))).collect()
        } else if root.is_some() {
            vec![(String::new(), lib.clone())]
        } else {
            continue;
        };
        for (user, projects) in users {
            scan(s, &projects, &user, &mut Vec::new(), 0, &mut budget, &mut out);
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

fn scan(s: &Session, dir: &str, user: &str, folder: &mut Vec<String>, depth: usize, budget: &mut usize, out: &mut Vec<Found>) {
    if depth > MAX_DEPTH || *budget == 0 {
        return;
    }
    *budget -= 1;
    let list = entries(s, dir);
    if let Some(db) = list.iter().find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(PROJECT_FILE)) {
        let name = folder.last().cloned().unwrap_or_default();
        let parents = folder.get(..folder.len().saturating_sub(1)).unwrap_or_default().to_vec();
        out.push(Found { name, user: user.to_string(), folder: parents, path: join(dir, &db.name), modified: db.modified, size: db.size });
        return;
    }
    for e in list.iter().filter(|e| e.is_dir && !e.name.starts_with('.')) {
        folder.push(e.name.clone());
        scan(s, &join(dir, &e.name), user, folder, depth + 1, budget, out);
        folder.pop();
    }
}

/// `file.listResolveProjects {root?}`
pub fn list(s: &Session, root: Option<&str>) -> Value {
    let found = find_projects(s, root);
    json!({
        "libraries": libraries(s, root),
        "projects": found.iter().map(|f| json!({
            "name": f.name,
            "user": f.user,
            "folder": f.folder,
            "path": f.path,
            "modified": f.modified,
            "size": f.size,
        })).collect::<Vec<_>>(),
    })
}

/// The `Project.db` of a project folder (or the path itself when it names a file).
pub fn database_path(s: &Session, path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let is_file = s.services.file_size(trimmed).is_ok();
    if is_file { trimmed.to_string() } else { join(trimmed, PROJECT_FILE) }
}

/// `file.importResolve {path}`: `path` is a project folder or its `Project.db`.
pub fn import(s: &mut Session, path: &str) -> Result<Value> {
    let db = database_path(s, path);
    let bytes = s.services.read_file(&db).map_err(|e| EngineError::Other(format!("{db}: {e}")))?;
    import_bytes(s, &db, &bytes)
}

/// Import the bytes of a `Project.db` read from `db`.
pub fn import_bytes(s: &mut Session, db: &str, bytes: &[u8]) -> Result<Value> {
    // the project folder's name, for a database that names no project
    let folder = std::path::Path::new(db).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    let opts = filmcraft_interchange::ImportOptions { name: folder, ..Default::default() };
    let (fragment, mut report) = filmcraft_interchange::resolve::import(bytes, &opts).map_err(|e| EngineError::Other(format!("{db}: {e}")))?;
    // Resolve writes recent changes to a write-ahead log first while the project is open
    if s.services.file_size(&format!("{db}-wal")).is_ok_and(|n| n > 0) {
        report.warn("DaVinci Resolve has changes that are not saved into Project.db yet (the project is probably open in Resolve): close it there and import again to include them");
    }
    let name = fragment.project.name.clone();
    crate::interchange::merge_fragment(s, db, fragment, report, filmcraft_interchange::resolve::FORMAT_NAME, Some(name), false)
}
