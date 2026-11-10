//! A failed fetch told in two levels (#509): what happened and what to do next in plain words,
//! and, folded under Details, what the server said, for an expert or the service's administrator.
//!
//! What the server said is untrusted: shown as plain text only, its address without keys.

use egui::{RichText, Ui, vec2};
use rustgis_engine::render::rustgis_tiles::failure::{Failure, Reason, status_name};

use rustgis_engine::sources::{address_in_message, message_with_address};

use crate::theme::Tokens;

/// What happened, and what to do next (translated).
pub fn plain(f: &Failure) -> (String, String) {
    let refused = tl!("The server refused the request.");
    match (f.reason, f.firewall()) {
        (Reason::Status(401), _) => (refused.into(), tl!("The service needs a sign-in or a token.").into()),
        // Only the firewall's own block: in front of a private service, it passes on that refusal.
        (Reason::Status(403), Some(firewall)) if f.firewall_blocked() => (
            refused.into(),
            crate::i18n::fmt(
                "Its firewall ({firewall}) blocked RustGIS. The service may only allow some applications: contact its administrator and send them the details below.",
                &[("firewall", firewall)],
            ),
        ),
        (Reason::Status(403), _) => (refused.into(), tl!("The service may be private or need a token.").into()),
        (Reason::Status(404), _) => (tl!("Nothing was found at this address.").into(), tl!("Check the address.").into()),
        (Reason::Status(429), _) => (tl!("The server is limiting requests.").into(), tl!("Try again in a moment.").into()),
        (Reason::Status(s), _) if s >= 500 => (tl!("The server had a problem answering.").into(), tl!("Try again later.").into()),
        (Reason::Status(_), _) => (refused.into(), tl!("Check the address and the service type.").into()),
        (Reason::Unreachable, _) => {
            (tl!("RustGIS couldn't reach the server.").into(), tl!("Check the address and your connection.").into())
        }
        (Reason::Timeout, _) => (tl!("The server took too long to answer.").into(), tl!("Try again later.").into()),
        (Reason::Other, _) => (tl!("The request failed.").into(), message_with_address(&f.url, &f.message)),
    }
}

/// What is known of the failure, as (label, value) rows: the labels in English, translated
/// where shown.
pub fn details(f: &Failure) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Address", address_in_message(&f.url))];
    match f.status() {
        Some(s) => rows.push(("Status", status_name(s).map_or_else(|| s.to_string(), |n| format!("{s} {n}")))),
        None => rows.push(("Error", message_with_address(&f.url, &f.message))),
    }
    let header = |k: &str| f.header(k).map(str::to_string);
    for (label, value) in [
        ("Server", header("server")),
        (
            "Firewall",
            f.firewall().map(|w| {
                if f.firewall_blocked() { format!("{w}, which blocked the request") } else { format!("{w}, in front of the server") }
            }),
        ),
        ("Request ID", f.request_id()),
        ("Sign-in", header("www-authenticate")),
        ("Retry after", header("retry-after")),
        ("Sent as", f.user_agent().map(|u| format!("User-Agent: {u}"))),
        ("Response", f.excerpt().map(|e| format!("\"{e}\""))),
    ] {
        if let Some(v) = value {
            rows.push((label, v));
        }
    }
    rows
}

/// The details as text to send to the service's administrator (in English, like the server's
/// own words).
pub fn report(f: &Failure) -> String {
    details(f).iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n")
}

/// The failure: its plain message, always shown, and its details, folded.
pub fn show(ui: &mut Ui, f: &Failure, t: &Tokens) {
    let (what, next) = plain(f);
    ui.label(RichText::new(what).strong().color(t.red));
    ui.label(next);
    egui::CollapsingHeader::new(tl!("Details")).id_salt("fetch_failure_details").default_open(false).show(ui, |ui| {
        egui::Grid::new("fetch_failure_rows").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
            for (label, value) in details(f) {
                ui.label(RichText::new(tl!(label)).color(t.text_dim));
                ui.add(egui::Label::new(RichText::new(value).monospace().size(11.5)).wrap());
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        if ui.button(tl!("Copy details")).clicked() {
            ui.ctx().copy_text(report(f));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_firewall_block_is_told_plainly_with_its_details() {
        let headers = [("server".to_string(), "cloudflare".to_string()), ("cf-ray".to_string(), "a4c9-BOG".to_string())];
        let page = b"<title>Attention Required! | Cloudflare</title>";
        let f = Failure::answered("https://example.org/MapServer?f=json&token=secret", 403, &headers, page).sent_as("RustGIS/1.0");
        let (what, next) = plain(&f);
        assert_eq!(what, "The server refused the request.");
        assert!(next.contains("Cloudflare") && next.contains("administrator"), "{next}");
        let report = report(&f);
        assert!(!report.contains("secret"), "keys are left out: {report}");
        for line in [
            "Status: 403 Forbidden",
            "Server: cloudflare",
            "Firewall: Cloudflare, which blocked the request",
            "Request ID: cf-ray a4c9-BOG",
            "Sent as: User-Agent: RustGIS/1.0",
            "Response: \"Attention Required! | Cloudflare\"",
        ] {
            assert!(report.contains(line), "{line} in {report}");
        }
    }

    /// A private service behind a firewall: its own refusal isn't blamed on the firewall.
    #[test]
    fn a_refusal_passed_on_by_a_firewall_is_the_service_s() {
        let headers = [("server".to_string(), "cloudflare".to_string()), ("cf-ray".to_string(), "a4c9-BOG".to_string())];
        let f = Failure::answered("u", 403, &headers, br#"{"error":{"code":403,"message":"Token Required"}}"#);
        let next = plain(&f).1;
        assert!(next.contains("token") && !next.contains("administrator"), "{next}");
        assert!(report(&f).contains("Firewall: Cloudflare, in front of the server"), "{}", report(&f));
    }

    #[test]
    fn each_kind_of_failure_says_what_to_do() {
        let status = |s: u16| plain(&Failure::answered("u", s, &[], b"")).1;
        assert!(status(401).contains("sign-in"));
        assert!(status(403).contains("private"));
        assert!(status(404).contains("address"));
        assert!(status(429).contains("moment"));
        assert!(status(502).contains("later"));
        assert!(status(418).contains("service type"));
        let unanswered = |r: Reason| plain(&Failure::without_answer("u", "x", r)).0;
        assert!(unanswered(Reason::Unreachable).contains("couldn't reach"));
        assert!(unanswered(Reason::Timeout).contains("too long"));
    }

    /// Known addresses are sanitized whole before either error surface composes the reason.
    #[test]
    fn addresses_in_an_error_lose_their_keys_without_losing_the_reason() {
        for url in [
            "https://alice:hidden@h.example/a?token=hidden unescaped hidden",
            "https://h.example/a?custom=hidden unescaped hidden",
            "https://h.example/a#hidden unescaped hidden",
        ] {
            let f = Failure::new(url, format!("{url}: the server doesn't answer byte-range requests"));
            let shown = plain(&f).1;
            let error = details(&f).into_iter().find(|(label, _)| *label == "Error").unwrap().1;
            for text in [shown, error, report(&f)] {
                let clean = rustgis_engine::Session::redacted(&rustgis_engine::Session::redacted(&text).into_string()).into_string();
                assert!(clean.contains("byte-range requests"), "{clean}");
                assert!(!clean.contains("hidden") && !clean.contains("alice"), "{clean}");
            }
        }
    }
}
