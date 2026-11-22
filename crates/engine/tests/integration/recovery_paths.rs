//! Recovery copies (#873) and portable projects (#886): a project saved with paths relative to its
//! folder, then reopened, gives a recovery copy whose linked files still resolve when the copy is
//! opened from the app's recovery folder.
use super::portable_projects::common::{Fixture, add_stream, sample_file, session};
use serde_json::json;

/// `path` as it is spelled inside project JSON.
fn in_json(path: &std::path::Path) -> String {
    let quoted = serde_json::to_string(&path.display().to_string()).unwrap();
    quoted.trim_matches('"').to_string()
}

#[test]
fn a_recovery_copy_keeps_linked_files_absolute() {
    let f = Fixture::new("recovery");
    let data = f.file("data/places.geojson");
    sample_file(&data, "Cities");
    let project = f.file("maps/atlas.rgis");
    let mut s = session();
    add_stream(&mut s, &data, None);
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    // The saved file is portable: the data's path is relative to the project's folder.
    let saved = std::fs::read_to_string(&project).unwrap();
    assert!(!saved.contains(&in_json(&data)), "saved relative");

    // Reopened, then copied for recovery: the copy names the data by its full path...
    let mut opened = session();
    opened.run("project.open", &json!({"path": project})).unwrap();
    let copy = opened.project.clone().recovery_text().unwrap();
    assert!(copy.contains(&in_json(&data)), "the copy keeps the data's full path");
    // ...so opened from elsewhere, as recovery does, it reads the same file.
    let mut recovered = session();
    recovered.run("project.openText", &json!({"text": copy})).unwrap();
    let live = serde_json::to_string(&recovered.project).unwrap();
    assert!(live.contains(&in_json(&data)));
    assert_eq!(recovered.map().layers.len(), opened.map().layers.len());
}
