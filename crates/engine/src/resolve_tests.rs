//! File ▸ Import From ▸ DaVinci Resolve Project: finding projects in a Resolve project library and
//! importing one end to end, with a real movie for its media.

use std::path::Path;

use filmcraft_project::{ItemKind, MediaRef};
use filmcraft_sqlite::Value;
use filmcraft_sqlite::write::Writer;
use filmcraft_time::FrameRate;
use serde_json::json;

use crate::media_test_util::{make_movie, tmp_dir};
use crate::{FsServices, Session};

fn t(s: &str) -> Value {
    Value::Text(s.into())
}

/// A one-timeline Resolve database: `movie` cut twice on V1 with its sound linked on A1, at
/// 01:00:00:00 and 24 fps.
fn database(movie: &str) -> Vec<u8> {
    let link = "(\"DbOwner\" uuid NOT NULL, \"DbAssociate\" uuid, \"DbPropertyName\" character varying NOT NULL, \"DbIndex\" integer NOT NULL)";
    let rate = [24f64.to_le_bytes(), [0; 8]].concat();
    let extents = [3600f64.to_le_bytes(), 2f64.to_le_bytes()].concat();
    let size = [64u64.to_be_bytes(), 36u64.to_be_bytes()].concat();
    let mut w = Writer::new(4096);
    w.table(
        "SM_Project",
        "CREATE TABLE \"SM_Project\" (\"SM_Project_id\" uuid NOT NULL, \"ProjectName\" character varying)",
        vec![vec![t("p"), t("Weekend Cut")]],
    );
    w.table(
        "Sm2Timeline",
        "CREATE TABLE \"Sm2Timeline\" (\"Sm2Timeline_id\" uuid NOT NULL, \"Name\" character varying, \"Sequence\" uuid)",
        vec![vec![t("tl"), t("Assembly"), t("s")]],
    );
    w.table(
        "Sm2Sequence",
        "CREATE TABLE \"Sm2Sequence\" (\"Sm2Sequence_id\" uuid NOT NULL, \"FrameRate\" BLOB, \"Resolution\" BLOB, \"MediaExtents\" BLOB)",
        vec![vec![t("s"), Value::Blob(rate.clone()), Value::Blob(size), Value::Blob(extents)]],
    );
    w.table(
        "Sm2SequenceContainer",
        "CREATE TABLE \"Sm2SequenceContainer\" (\"Sm2SequenceContainer_id\" uuid NOT NULL, \"Sm2Sequence_id\" uuid)",
        vec![vec![t("c"), t("s")]],
    );
    let tl = |o: &str, p: &str, i: i64, a: &str| vec![t(o), t(a), t(p), Value::Integer(i)];
    w.table(
        "Sm2SequenceContainer_Sm2TiTrack",
        &format!("CREATE TABLE \"Sm2SequenceContainer_Sm2TiTrack\" {link}"),
        vec![tl("c", "VideoTrackVec", 0, "v1"), tl("c", "AudioTrackVec", 0, "a1")],
    );
    w.table(
        "Sm2TiTrack",
        "CREATE TABLE \"Sm2TiTrack\" (\"Sm2TiTrack_id\" uuid NOT NULL, \"Type\" integer)",
        vec![vec![t("v1"), Value::Integer(0)], vec![t("a1"), Value::Integer(1)]],
    );
    let item = |id: &str, ty: &str, start: i64, dur: i64, inn: i64, track: &str| {
        vec![t(id), t(ty), t("clip"), t(&start.to_string()), t(&dur.to_string()), t(&inn.to_string()), t("m"), t(movie), Value::Blob(rate.clone()), t(track)]
    };
    w.table(
        "Sm2TiItem",
        "CREATE TABLE \"Sm2TiItem\" (\"Sm2TiItem_id\" uuid NOT NULL, \"DbType\" character varying, \"Name\" character varying, \"Start\" character varying, \"Duration\" character varying, \"In\" character varying, \"MediaRef\" uuid, \"MediaFilePath\" character varying, \"MediaFrameRate\" BLOB, \"Sm2TiTrack_id\" uuid)",
        vec![
            item("i1", "Sm2TiVideoClip", 86400, 12, 0, "v1"),
            item("i2", "Sm2TiVideoClip", 86412, 12, 24, "v1"),
            item("i3", "Sm2TiAudioClip", 86400, 12, 0, "a1"),
            item("i4", "Sm2TiAudioClip", 86412, 12, 24, "a1"),
        ],
    );
    w.finish()
}

/// `<root>/Resolve Projects/Users/guest/Projects/Shorts/Weekend Cut/Project.db`, with its movie.
fn library(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = tmp_dir(name);
    let movie = root.join("a.mov");
    make_movie(&movie, filmcraft_media::DemoScene::Aurora, 64, 36, 48);
    let project = root.join("Resolve Projects").join("Users").join("guest").join("Projects").join("Shorts").join("Weekend Cut");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("Project.db"), database(&movie.to_string_lossy())).unwrap();
    (root, project)
}

fn session() -> Session {
    Session::new(std::sync::Arc::new(FsServices))
}

fn p(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn lists_the_projects_of_a_library() {
    let (root, project) = library("resolve-list");
    let mut s = session();
    let v = s.execute("file.listResolveProjects", json!({ "root": p(&root) })).unwrap();
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{v}");
    assert_eq!(projects[0]["name"], "Weekend Cut");
    assert_eq!(projects[0]["user"], "guest");
    assert_eq!(projects[0]["folder"], json!(["Shorts"]));
    assert_eq!(projects[0]["path"].as_str().map(Path::new), Some(project.join("Project.db").as_path()));
    // an empty or missing library lists nothing
    let v = s.execute("file.listResolveProjects", json!({ "root": p(&root.join("nothing")) })).unwrap();
    assert_eq!(v["projects"], json!([]));
}

#[test]
fn imports_a_project_folder_and_links_its_media() {
    let (root, project) = library("resolve-import");
    let mut s = session();
    let before = s.project.items.len();
    let r = s.execute("file.importResolve", json!({ "path": p(&project) })).unwrap();
    assert_eq!(r["format"], "DaVinci Resolve");
    assert_eq!(r["linkedMedia"], 1, "{r}");
    assert_eq!(r["offlineMedia"], json!([]));
    let seq_id = filmcraft_project::ItemId(r["sequences"][0].as_u64().unwrap());
    assert_eq!(s.state.active_sequence, Some(seq_id));
    let item = s.project.item(seq_id).unwrap();
    assert_eq!(item.name, "Assembly");
    let seq = s.project.sequence(seq_id).unwrap();
    let fps = FrameRate::FPS_24;
    assert_eq!(seq.start_timecode, 86400);
    let v1 = &seq.video_tracks[0].items;
    assert_eq!((v1[0].start, v1[1].start, v1[1].source_in), (fps.tick_of(0), fps.tick_of(12), fps.tick_of(24)));
    assert!(v1[0].link.is_some() && v1[0].link == seq.audio_tracks[0].items[0].link);
    let ItemKind::Media(m) = &s.project.item(v1[0].item).unwrap().kind else { panic!("not media") };
    assert_eq!(m.media, MediaRef::File { path: p(&root.join("a.mov")) });
    assert!(!m.offline);
    assert_eq!(m.info.video.as_ref().map(|v| (v.width, v.height)), Some((64, 36)));
    // undo Link Media, then the import itself (as for every interchange import)
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(seq_id).is_some());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(seq_id).is_none());
    assert!(s.project.items.len() <= before + 1);
}

#[test]
fn file_import_recognises_a_project_database() {
    let (_root, project) = library("resolve-file-import");
    let mut s = session();
    let r = s.execute("file.import", json!({ "paths": [p(&project.join("Project.db"))] })).unwrap();
    assert_eq!(r["sequences"].as_array().map(Vec::len), Some(1), "{r}");
    // and a project folder dropped as is
    let r = s.execute("file.import", json!({ "paths": [p(&project)] })).unwrap();
    assert_eq!(r["sequences"].as_array().map(Vec::len), Some(1), "{r}");
}

#[test]
fn warns_about_changes_still_in_the_write_ahead_log() {
    let (_root, project) = library("resolve-wal");
    std::fs::write(project.join("Project.db-wal"), [1u8; 64]).unwrap();
    let mut s = session();
    let r = s.execute("file.importResolve", json!({ "path": p(&project.join("Project.db")) })).unwrap();
    assert!(r["report"].as_array().unwrap().iter().any(|m| m.as_str().unwrap().contains("not saved into Project.db yet")), "{r}");
}

#[test]
fn reports_what_is_not_a_resolve_project() {
    let dir = tmp_dir("resolve-bad");
    let mut s = session();
    assert!(s.execute("file.importResolve", json!({ "path": p(&dir.join("missing")) })).is_err());
    let mut w = Writer::new(4096);
    w.table("notes", "CREATE TABLE notes (a)", vec![vec![t("x")]]);
    std::fs::write(dir.join("Project.db"), w.finish()).unwrap();
    let e = s.execute("file.importResolve", json!({ "path": p(&dir) })).unwrap_err().to_string();
    assert!(e.contains("not a DaVinci Resolve project"), "{e}");
    std::fs::write(dir.join("Project.db"), b"garbage").unwrap();
    assert!(s.execute("file.importResolve", json!({ "path": p(&dir) })).is_err());
    assert!(s.execute("file.importResolve", json!({})).is_err());
}
