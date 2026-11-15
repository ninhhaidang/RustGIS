//! What a phone provides that a computer's window doesn't: the system's file picker, the device's
//! position and the camera. A mobile shell implements [`Platform`] for its system; a front end
//! asks through it and collects what happened with [`Platform::take_events`], as nothing here
//! waits for the user.

use std::path::PathBuf;

/// Where the device is, as its system reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    /// Longitude, in degrees (WGS 84).
    pub lon: f64,
    /// Latitude, in degrees (WGS 84).
    pub lat: f64,
    /// The radius the system gives the position to (about two times in three), in metres, when
    /// it says.
    pub accuracy: Option<f64>,
    /// When the system found it, in milliseconds since 1970 (UTC), when it says.
    pub time: Option<i64>,
}

/// What the system did since the front end last asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A file the user picked, copied to where it can be opened by path.
    Picked {
        /// What [`Platform::pick_files`] was asked for.
        purpose: String,
        /// The file's name, as the user saw it.
        name: String,
        /// Where its copy is.
        path: PathBuf,
    },
    /// A picker closed without a choice.
    Cancelled,
    /// A request for files ([`Platform::pick_files`]'s `purpose`) ended with none: its picker
    /// answered, but no file of it could be copied (each failure said beside this). Unlike
    /// [`Event::Cancelled`], which is the picker on the screen, it names the request, as another
    /// may have been asked for while its files were being copied.
    Ended {
        /// What the request was asked for.
        purpose: String,
    },
    /// A file was written where the user chose.
    Saved {
        /// The name it was offered under.
        name: String,
    },
    /// A photo the camera took ([`Platform::take_photo`]), written where it can be opened by path.
    Photo {
        /// The photo's file name.
        name: String,
        /// Where it is.
        path: PathBuf,
    },
    /// The device's position, while [`Platform::locate`] is on.
    Position(Fix),
    /// Positioning ended without [`Platform::locate`] turning it off (leave refused or taken
    /// away, no way to find a position): the reason comes as an [`Event::Failed`].
    Stopped,
    /// Something went wrong: the text is for the user.
    Failed(String),
}

impl Event {
    /// Reads an event a shell passed as one line of tab-separated fields (`picked`, `cancelled`,
    /// `ended`, `saved`, `photo`, `position`, `stopped`, `failed`). A line that is none of these is a failure that quotes it: nothing a
    /// shell reports is dropped. A position is `position`, longitude, latitude, accuracy (empty
    /// when unknown) and, optionally, its time in milliseconds since 1970.
    pub fn parse(line: &str) -> Event {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["picked", purpose, name, path] => {
                Event::Picked { purpose: purpose.to_string(), name: name.to_string(), path: PathBuf::from(path) }
            }
            ["cancelled"] => Event::Cancelled,
            ["ended", purpose] => Event::Ended { purpose: purpose.to_string() },
            ["photo", name, path] if !path.is_empty() => Event::Photo { name: name.to_string(), path: PathBuf::from(path) },
            ["stopped"] => Event::Stopped,
            // A name is the user's own text and may hold a tab: the rest of the line is the name.
            ["saved", name @ ..] if !name.is_empty() => Event::Saved { name: name.join("\t") },
            ["position", lon, lat, accuracy, rest @ ..] if rest.len() <= 1 => {
                let unknown = || Event::Failed(format!("The system reported something RustGIS doesn't know: {line}"));
                // A time given that isn't one is a line RustGIS doesn't know, as below.
                let time = match rest.first() {
                    None => None,
                    Some(ms) => match ms.parse::<i64>() {
                        Ok(ms) if ms >= 0 => Some(ms),
                        _ => return unknown(),
                    },
                };
                match (lon.parse::<f64>(), lat.parse::<f64>()) {
                    // A place on the Earth, or it is no position: a failure that quotes the line.
                    (Ok(lon), Ok(lat)) if (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat) => {
                        // No accuracy given is no accuracy; one given that isn't a distance is a
                        // line RustGIS doesn't know, not a position quietly taken without it.
                        match accuracy.parse::<f64>() {
                            _ if accuracy.is_empty() => Event::Position(Fix { lon, lat, accuracy: None, time }),
                            Ok(metres) if metres.is_finite() && metres >= 0.0 => {
                                Event::Position(Fix { lon, lat, accuracy: Some(metres), time })
                            }
                            _ => unknown(),
                        }
                    }
                    _ => unknown(),
                }
            }
            ["failed", text @ ..] => Event::Failed(text.join("\t")),
            _ => Event::Failed(format!("The system reported something RustGIS doesn't know: {line}")),
        }
    }
}

/// The phone, as a front end sees it.
pub trait Platform {
    /// Asks the user for files with the system's picker. `purpose` says what they are for (the
    /// front end's own word, returned with each file); each file arrives as an [`Event::Picked`],
    /// a closed picker as [`Event::Cancelled`].
    fn pick_files(&self, purpose: &str) -> Result<(), String>;

    /// Asks the user where to write `bytes`, offered under `name`, and writes them there:
    /// [`Event::Saved`] or [`Event::Failed`] follows. `Ok` means the user was asked, not that the
    /// file is written.
    fn save_file(&self, name: &str, bytes: Vec<u8>) -> Result<(), String>;

    /// Starts (`on`) or stops reporting the device's position: each fix arrives as an
    /// [`Event::Position`]. Starting asks the user for leave where the system wants it. A refusal,
    /// like any other end to positioning that wasn't asked for, arrives as an [`Event::Failed`]
    /// and then an [`Event::Stopped`], so the interface knows no position is coming. `Ok` means
    /// it was asked, not that a position is known.
    fn locate(&self, on: bool) -> Result<(), String>;

    /// Keeps positions coming while the app is out of sight (`on`), for a track being recorded,
    /// or lets them stop there again. Where the system wants it, this shows a notice for as long
    /// as it lasts (Android's foreground service). The positions found out of sight arrive
    /// together when the app is back. Asked while the app is in sight.
    fn keep_locating(&self, on: bool) -> Result<(), String>;

    /// Asks the device's camera for a photo: an [`Event::Photo`] follows with where it was
    /// written (in the app's own storage), or [`Event::Cancelled`] when there is none (the user
    /// took none, or it couldn't be kept). `Ok` means the camera was asked, not that a photo was
    /// taken.
    fn take_photo(&self) -> Result<(), String>;

    /// What happened since the last call, oldest first.
    fn take_events(&self) -> Vec<Event>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_from_their_lines() {
        assert_eq!(
            Event::parse("picked\tdata\tparcels.gpkg\t/data/picked/1/parcels.gpkg"),
            Event::Picked { purpose: "data".into(), name: "parcels.gpkg".into(), path: PathBuf::from("/data/picked/1/parcels.gpkg") }
        );
        assert_eq!(Event::parse("cancelled"), Event::Cancelled);
        assert_eq!(Event::parse("ended\tdxf:3"), Event::Ended { purpose: "dxf:3".into() });
        assert_eq!(
            Event::parse("photo\tphoto-1.jpg\t/data/photos/photo-1.jpg"),
            Event::Photo { name: "photo-1.jpg".into(), path: PathBuf::from("/data/photos/photo-1.jpg") }
        );
        assert_eq!(Event::parse("stopped"), Event::Stopped);
        assert_eq!(
            Event::parse("position\t-83.92\t35.96\t4.5"),
            Event::Position(Fix { lon: -83.92, lat: 35.96, accuracy: Some(4.5), time: None })
        );
        // No accuracy given: the position stands without it.
        assert_eq!(Event::parse("position\t10\t20\t"), Event::Position(Fix { lon: 10.0, lat: 20.0, accuracy: None, time: None }));
        // With its time.
        assert_eq!(
            Event::parse("position\t10\t20\t3\t1794787200000"),
            Event::Position(Fix { lon: 10.0, lat: 20.0, accuracy: Some(3.0), time: Some(1_794_787_200_000) })
        );
        assert_eq!(Event::parse("saved\tmap.png"), Event::Saved { name: "map.png".into() });
        assert_eq!(Event::parse("saved\tmy\tmap.png"), Event::Saved { name: "my\tmap.png".into() });
        // A failure's text is kept whole, tabs and all.
        assert_eq!(Event::parse("failed\ta.gpkg wasn't opened:\tno room"), Event::Failed("a.gpkg wasn't opened:\tno room".into()));
    }

    #[test]
    fn an_unknown_line_is_a_failure_that_quotes_it() {
        for line in [
            "",
            "picked\tdata",
            "ended",
            "moved\tx",
            "saved",
            "photo\ta.jpg",
            "photo\ta.jpg\t",
            "position\t200\t20\t5",
            "position\tx\t20\t5",
            "position\tNaN\t20\t5",
            "position\t10\t20\t-1",
            "position\t10\t20\tabc",
            "position\t10\t20\tinf",
            "position\t10\t20\t3\tsoon",
            "position\t10\t20\t3\t-5",
            "position\t10\t20\t3\t1\t2",
        ] {
            match Event::parse(line) {
                Event::Failed(text) => assert!(text.ends_with(line), "{text}"),
                other => panic!("{line:?} read as {other:?}"),
            }
        }
    }
}
