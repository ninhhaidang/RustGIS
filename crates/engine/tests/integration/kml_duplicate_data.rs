use base64::Engine;
use rustgis_engine::{Project, Session};
use serde_json::json;

#[test]
fn kml_duplicate_extended_data_reaches_session_notices() {
    for duplicate in [false, true] {
        let extra = if duplicate { r#"<Data name="n"><value>5</value></Data>"# } else { "" };
        let text = format!(
            r#"<kml><Placemark><ExtendedData><Data name="n"><value>0</value></Data>{extra}</ExtendedData><Point><coordinates>1,2</coordinates></Point></Placemark></kml>"#
        );
        for archived in [false, true] {
            let (name, bytes) = if archived {
                ("sample.kmz", rustgis_engine::data::zip::write(&[("doc.kml".into(), text.as_bytes().to_vec())]).unwrap())
            } else {
                ("sample.kml", text.as_bytes().to_vec())
            };
            let mut s = Session::new(Project::default());
            let mark = s.notice_mark();
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            s.run("layer.addDataBytes", &json!({"fileName": name, "data": data})).unwrap();
            let notices: Vec<_> = s.notices_since(mark).iter().filter(|n| n.text.contains("repeated ExtendedData")).collect();
            assert_eq!(notices.len(), usize::from(duplicate));
            if let Some(notice) = notices.first() {
                assert!(notice.text.contains("1 repeated ExtendedData entries in 1 placemarks"));
            }
        }
    }
}
