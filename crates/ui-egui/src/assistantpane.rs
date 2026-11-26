//! The AI Assistant pane: a chat with a model of the user's choosing, which acts on the map by
//! running commands (see the `rustgis-assistant` crate). This is its desktop layout; the
//! conversation and what can be done with it are in [`crate::assistant`].

use egui::{Align, Layout, RichText, Ui, vec2};
use rustgis_assistant::{Config, Entry, PRESETS, chatgpt, provider};

#[cfg(not(target_arch = "wasm32"))]
use crate::assistant::pref;
use crate::assistant::{self, Approval, FIELDS, key_loading, save_pref, send};
use crate::theme::{Tokens, regular, semibold};
use crate::{RustGisApp, icons};

/// Example requests offered in an empty conversation.
const SUGGESTIONS: &[&str] = &[
    "Which 5 countries have the largest GDP?",
    "Zoom to Tokyo",
    "Label the cities with their names",
    "Buffer the cities by 200 km",
    "Switch to a dark basemap",
];

/// Draws the Assistant pane, loading its provider on first use.
pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    assistant::open(app, ui.ctx());
    let ctx = ui.ctx().clone();
    let busy = app.assistant.busy();
    // The model and settings, at the top.
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (label, model) = app
                .assistant
                .config()
                .map(|c| (provider::preset(&c.provider).map_or("", |p| p.label), c.model.clone()))
                .unwrap_or_default();
            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), egui::Sense::hover());
            icons::paint(ui.painter(), r, "assistant", t.text_dim, t.accent);
            let model_label = egui::Label::new(RichText::new(format!("{} · {}", tl!(label), model)).small().color(t.text_dim))
                .truncate()
                .sense(egui::Sense::click());
            // Room for the two buttons; a long model name is cut short.
            let w = (ui.available_width() - 2.0 * (26.0 + ui.spacing().item_spacing.x)).max(0.0);
            if ui
                .allocate_ui(vec2(w, 16.0), |ui| ui.add(model_label))
                .inner
                .on_hover_text(tl!("The model answering; click to change it"))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                assistant::toggle_settings(app);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if crate::widgets::icon_button(ui, "settings", tl!("Assistant settings")).clicked() {
                    assistant::toggle_settings(app);
                }
                let has_chat = !app.assistant.messages().is_empty();
                if has_chat && crate::widgets::icon_button(ui, "delete", tl!("New conversation")).clicked() {
                    assistant::new_conversation(app);
                }
                if has_chat
                    && !app.assistant.busy()
                    && ui
                        .small_button(tl!("Compact"))
                        .on_hover_text(tl!("Summarise the conversation so far, so a long chat stays within the model's context"))
                        .clicked()
                {
                    assistant::compact(app, &ctx);
                }
            });
        });
    });
    // The composer first, so tall settings scroll above it instead of running under it.
    composer_panel(app, ui, t, busy, &ctx);
    // Closed settings look for the agents again when they next open.
    #[cfg(not(target_arch = "wasm32"))]
    if !app.assistant.settings {
        app.assistant.agents_found = None;
    }
    if app.assistant.settings {
        egui::ScrollArea::vertical()
            .id_salt("assistant_settings_scroll")
            .auto_shrink([false, true])
            .min_scrolled_height(0.0)
            .max_height((ui.available_height() - 8.0).max(0.0))
            .show(ui, |ui| {
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 4)).show(ui, |ui| settings(app, ui, t));
            });
        ui.separator();
    }
    // The conversation in between.
    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).min_scrolled_height(0.0).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            // Text in a row (a list item, a quote, the user's message) wraps too, instead of widening the pane.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            transcript(app, ui, t, &ctx);
        });
    });
}

fn composer_panel(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, busy: bool, ctx: &egui::Context) {
    egui::Panel::bottom("assistant_composer")
        .frame(egui::Frame::NONE.fill(t.pane).inner_margin(egui::Margin::symmetric(10, 8)))
        .show(ui, |ui| composer(app, ui, t, busy, ctx));
}

/// Edits provider settings and invalidates pending key reads as soon as a key changes.
fn settings(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    #[cfg(not(target_arch = "wasm32"))]
    assistant::poll_sign_in(app, ui.ctx());
    let Some(mut cfg) = app.assistant.config().cloned() else { return };
    let before = (cfg.clone(), app.assistant.remember_key);
    let open = !app.assistant.provider_closed;
    let section = egui::CollapsingHeader::new(RichText::new(tl!("AI provider")).font(semibold(12.5)))
        .id_salt("assistant_provider_section")
        .open(Some(open))
        .show(ui, |ui| provider_settings(app, ui, t, &mut cfg));
    if section.header_response.clicked() {
        assistant::fold_provider(app, open);
    }
    if !key_loading(app)
        && let Some(why) = cfg.missing()
    {
        ui.label(RichText::new(tl!(why)).small().color(t.amber));
    }
    #[cfg(not(target_arch = "wasm32"))]
    local_agents_setting(app, ui, t);
    #[cfg(not(target_arch = "wasm32"))]
    skills_setting(app, ui, t);
    // Agents on this computer working on this map through the MCP relay (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    crate::connectui::section(app, ui, t);
    assistant::apply_settings(app, cfg, before, ui.ctx());
}

/// The settings' AI provider section: the provider, account, model, address and key, with
/// notes on where they're kept and what is sent.
fn provider_settings(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, cfg: &mut Config) {
    egui::Grid::new("assistant_settings").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        let provider_label = ui.label(tl!("Provider"));
        let current = provider::preset(&cfg.provider).map_or("", |p| p.label);
        egui::ComboBox::from_id_salt("assistant_provider")
            .selected_text(tl!(current))
            .width(180.0)
            .show_ui(ui, |ui| {
                for p in PRESETS {
                    // Signing in needs the browser and a server on this computer: the desktop app.
                    if p.login && cfg!(target_arch = "wasm32") {
                        continue;
                    }
                    if ui.selectable_label(cfg.provider == p.id, tl!(p.label)).clicked() && cfg.provider != p.id {
                        assistant::choose_provider(app, cfg, p.id, ui.ctx());
                    }
                }
            })
            .response
            .labelled_by(provider_label.id);
        ui.end_row();
        if cfg.login() {
            ui.label(tl!("Account"));
            ui.vertical(|ui| account_row(app, ui, t, cfg));
            ui.end_row();
        }
        let model_label = ui.label(tl!("Model"));
        let signed_out = cfg.login() && cfg.key.is_empty();
        ui.add_enabled_ui(!signed_out, |ui| {
            ui.horizontal(|ui| {
                if signed_out {
                    ui.label(tl!("Sign in to choose a model."));
                    return;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.model)
                        .id(egui::Id::unique(FIELDS[0]))
                        .desired_width(150.0)
                        .hint_text(tl!("model id")),
                )
                .labelled_by(model_label.id);
                if !app.assistant.models.is_empty() {
                    egui::ComboBox::from_id_salt("assistant_models")
                        .selected_text("")
                        .width(24.0)
                        .height(320.0)
                        .show_ui(ui, |ui| {
                            for m in &app.assistant.models {
                                if ui
                                    .selectable_label(
                                        cfg.model == *m,
                                        app.assistant.catalog.iter().find(|c| c.id == *m).map_or(m.as_str(), |c| c.name.as_str()),
                                    )
                                    .clicked()
                                {
                                    cfg.model = m.clone();
                                }
                            }
                        })
                        .response
                        .on_hover_text(tl!("Model"));
                }
                let loading = app.assistant.models_slot.is_some();
                if ui
                    .add_enabled(!loading, egui::Button::new(if loading { "…" } else { "↻" }))
                    .on_hover_text(tl!("List the provider's models"))
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    assistant::list_models(app, cfg, &ctx);
                }
            });
        });
        ui.end_row();
        let preset = provider::preset(&cfg.provider);
        if !cfg.login() {
            let address_label = ui.label(tl!("Address"));
            ui.add(egui::TextEdit::singleline(&mut cfg.base_url).id(egui::Id::unique(FIELDS[1])).desired_width(200.0))
                .labelled_by(address_label.id);
            ui.end_row();
            let key_label = ui.label(tl!("API key"));
            ui.horizontal(|ui| {
                let hint = if preset.is_some_and(|p| p.keyless) { tl!("not needed") } else { tl!("paste your key") };
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.key)
                        .id(egui::Id::unique(FIELDS[2]))
                        .password(!app.assistant.show_key)
                        .desired_width(150.0)
                        .hint_text(hint),
                )
                .labelled_by(key_label.id);
                let eye = if app.assistant.show_key { tl!("Hide") } else { tl!("Show") };
                if ui.small_button(eye).clicked() {
                    app.assistant.show_key = !app.assistant.show_key;
                }
            });
        }
        ui.end_row();
    });
    let preset = provider::preset(&cfg.provider);
    let mut notes = Vec::new();
    if cfg.login() {
        notes.push(sign_in_storage_note(app.assistant.account.as_ref().filter(|_| !cfg.key.is_empty())));
        notes.push(tl!("Requests go to the OpenAI API and use your ChatGPT plan.").to_string());
    } else {
        ui.checkbox(&mut app.assistant.remember_key, tl!("Remember the key on this computer"));
        if let Some(p) = preset.filter(|p| !p.key_env.is_empty()) {
            notes.push(crate::i18n::fmt("On the desktop, the key can also come from {vars}.", &[("vars", &p.key_env.join(", "))]));
        }
        notes.push(key_storage_note(app, &cfg.provider));
    }
    notes.push(
        tl!("Requests send your message and the map's layer names, fields and the rows the assistant reads to the provider.").to_string(),
    );
    for n in notes {
        ui.label(RichText::new(n).small().color(t.text_dim));
    }
    if let Some(p) = preset.filter(|p| !p.key_url.is_empty() && !p.login) {
        ui.hyperlink_to(RichText::new(if p.keyless { tl!("Get it") } else { tl!("Get a key") }).small(), p.key_url);
    }
    if let Some(e) = &app.assistant.models_error {
        show_error(ui, e, t);
    }
    if cfg.login() && !cfg.key.is_empty() {
        plan_notice(app, ui, t);
    }
    if let Some(e) = &app.assistant.login_error {
        show_error(ui, e, t);
    }
}

/// Where the ChatGPT sign-in is kept, as truthfully as is known.
fn sign_in_storage_note(account: Option<&chatgpt::Account>) -> String {
    match account.map(|a| a.kept) {
        Some(chatgpt::Kept::SettingsFile) => tl!(
            "The sign-in is still kept in the app's settings folder, readable by your user account: the system keychain is unavailable, so it hasn't been moved there yet."
        ),
        Some(chatgpt::Kept::Session) => tl!(
            "The system keychain is unavailable, so this sign-in won't be remembered: it lasts until RustGIS closes."
        ),
        _ => tl!("The sign-in is kept in the system keychain, never in the app's settings or a project file."),
    }
    .to_string()
}

/// Describes actual storage, including a legacy key retained after a failed migration.
fn key_storage_note(app: &RustGisApp, provider: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = provider;
        if app.assistant.remember_key {
            tl!("Saved keys are kept unencrypted in the browser's storage.").to_string()
        } else {
            tl!("The key is kept for this session only.").to_string()
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::assistantkeys::Status;
        use rustgis_engine::data::secrets::SecretError;
        let mut notes = Vec::new();
        let plaintext = pref(app, &format!("assistant_key_{provider}")).is_some();
        match app.assistant.keys.status(provider) {
            Some(Status::Stored) if app.assistant.remember_key => notes.push(tl!("The key is saved in the system keychain.")),
            Some(Status::Pending { .. }) => notes.push(tl!("Accessing the system keychain…")),
            Some(Status::Failed { error, legacy, deleting, reading }) => {
                notes.push(match error {
                    SecretError::Denied => tl!("The system keychain is locked or access was refused."),
                    SecretError::Unavailable(_) => tl!("The system keychain is unavailable."),
                    _ => tl!("The system keychain could not complete the request."),
                });
                // With no store here (a phone without one), nothing was ever saved to be read
                // or deleted.
                let store = app.services.secrets.is_some();
                if *reading && !store {
                    notes.push(tl!("A key entered now will be kept for this session only."));
                } else if *reading {
                    notes.push(tl!("A saved key could not be read. A key entered now will be kept for this session only."));
                } else if *deleting && store {
                    notes.push(tl!("RustGIS could not confirm that any previously saved key was deleted from the system keychain."));
                } else if !legacy || !plaintext {
                    notes.push(tl!("The key is kept for this session only; it has not been remembered."));
                }
            }
            _ => notes.push(tl!("The key is kept for this session only.")),
        }
        if plaintext {
            notes.push(tl!(
                "A previously saved key is still kept unencrypted in the app's preferences until it can be moved to the system keychain."
            ));
        }
        notes.join("\n")
    }
}

/// The settings' account row of a sign-in provider: who is signed in and Sign out, or
/// Continue with ChatGPT (the browser).
#[cfg(not(target_arch = "wasm32"))]
fn account_row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, cfg: &mut Config) {
    if let Some(login) = &app.assistant.login {
        let (reopen, cancel) = waiting_for_browser(ui, t, &login.url);
        if reopen {
            app.ui.open_url = Some(login.browser_url.clone());
        }
        if cancel {
            app.assistant.login = None;
        }
        return;
    }
    let saved = assistant::accounts(app);
    let active = saved.iter().find(|c| c.client_id == cfg.client_id && c.subject == cfg.account);
    let label = active.map(chatgpt::Credentials::label).unwrap_or_else(|| tl!("Choose an account").into());
    let picker = egui::ComboBox::from_id_salt("assistant_account").selected_text(label).width(180.0).truncate().show_ui(ui, |ui| {
        for account in &saved {
            let selected = active.as_ref().is_some_and(|c| c.client_id == account.client_id && c.subject == account.subject);
            // Selecting the current row also recovers a window with an outdated sign-in.
            if ui.selectable_label(selected, account.label()).clicked() {
                assistant::choose_account(app, cfg, account, ui.ctx());
            }
        }
    });
    picker.response.clone().on_hover_text(tl!("Choose an account"));
    // Opening the list reads it again: another window or the command line may have added one.
    if picker.response.clicked() {
        app.assistant.accounts = None;
    }
    let another = ui.small_button(tl!("Use another account")).clicked();
    if another {
        assistant::start_login(app, cfg, true);
        return;
    }
    let selected = active.map(|c| (c.client_id.clone(), c.subject.clone()));
    if let Some((client_id, subject)) = selected
        && ui
            .small_button(tl!("Remove account"))
            .on_hover_text(tl!("Signs this account out and forgets it on this computer, with its saved sign-in."))
            .clicked()
    {
        assistant::remove_account(app, cfg, &client_id, &subject, ui.ctx());
        return;
    }
    if !cfg.key.is_empty() {
        let who = app.assistant.account.as_ref().map(|a| a.email.clone()).filter(|e| !e.is_empty());
        ui.label(RichText::new(who.unwrap_or_else(|| tl!("signed in").to_string())).small());
        if ui.small_button(tl!("Sign out")).clicked() && !assistant::sign_out(app, cfg, ui.ctx()) {
            return;
        }
        if app.assistant.duplicate {
            ui.label(
                RichText::new(tl!("This ChatGPT account was already saved. Next time, choose it in the list instead of adding it again."))
                    .small()
                    .color(t.text_dim),
            );
        }
        return;
    }
    if ui.button(tl!("Continue with ChatGPT")).clicked() {
        assistant::start_login(app, cfg, false);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn waiting_for_browser(ui: &mut Ui, t: &Tokens, url: &str) -> (bool, bool) {
    ui.spinner();
    ui.label(RichText::new(tl!("Waiting for the browser…")).small().color(t.text_dim));
    let reopen = ui.small_button(tl!("Open browser again")).clicked();
    if ui.small_button(tl!("Copy sign-in link")).clicked() {
        ui.ctx().copy_text(url.to_string());
    }
    (reopen, ui.small_button(tl!("Cancel")).clicked())
}

/// Whether the assistant may hand tasks to the agents on this computer (Claude Code, Codex,
/// Gemini CLI…) through ACP. Off unless the user turns it on; each agent's actions then wait for
/// the user's approval, unless they choose otherwise.
#[cfg(not(target_arch = "wasm32"))]
fn local_agents_setting(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if let Some(a) = app.assistant.agent.as_mut() {
        a.set_agents_auto(app.assistant.agents_auto);
    }
    if ui.checkbox(&mut app.assistant.local_agents, tl!("Let the assistant hand tasks to agents on this computer")).changed() {
        app.assistant.agents_found = None;
        let (found, _) = app.assistant.agents_found.get_or_insert_with(rustgis_assistant::acp::installed);
        let found: Vec<_> = found.iter().map(|(spec, _)| *spec).collect();
        save_pref(app, "assistant_local_agents", if app.assistant.local_agents { "on" } else { "off" });
        let agents = if app.assistant.local_agents { found } else { Vec::new() };
        if let Some(a) = app.assistant.agent.as_mut() {
            a.set_harnesses(agents);
        }
    }
    let (found, need_node) = app.assistant.agents_found.get_or_insert_with(rustgis_assistant::acp::installed).clone();
    let note = if found.is_empty() {
        tl!("No agents that speak ACP (Claude Code, Codex, Gemini CLI, GitHub Copilot CLI, Cursor, OpenCode, goose) were found on this computer.").to_string()
    } else {
        let names: Vec<&str> = found.iter().map(|(spec, _)| spec.label).collect();
        crate::i18n::fmt(
            "Found: {names}. An agent works on this map through RustGIS's tools and with its own (web, scripts, files) in a folder of its own, kept in Documents › RustGIS › Agent work. It asks you before each action.",
            &[("names", &names.join(", "))],
        )
    };
    ui.label(RichText::new(note).small().color(t.text_dim));
    if found.iter().any(|(spec, command)| {
        spec.adapter.is_some() && command.first().is_some_and(|c| std::path::Path::new(c).file_stem().is_some_and(|s| s == "npx"))
    }) {
        ui.label(
            RichText::new(tl!(
                "Claude Code and Codex connect through an ACP adapter. On first use, npx downloads and runs its pinned version from npm."
            ))
            .small()
            .color(t.text_dim),
        );
    }
    if !need_node.is_empty() {
        let names: Vec<&str> = need_node.iter().map(|spec| spec.label).collect();
        ui.label(
            RichText::new(crate::i18n::fmt("{names}: needs Node.js (npx) for its ACP adapter.", &[("names", &names.join(", "))]))
                .small()
                .color(t.amber),
        );
    }
    if app.assistant.local_agents && ui.checkbox(&mut app.assistant.agents_auto, tl!("Approve agents' actions without asking")).changed() {
        // Publish the change before returning from the widget: a held ACP reply can arrive
        // before the next frame polls threads.
        app.assistant.threads.set_auto(app.assistant.agents_auto);
        save_pref(app, "assistant_agents_auto", if app.assistant.agents_auto { "on" } else { "off" });
        if let Some(a) = app.assistant.agent.as_mut() {
            a.set_agents_auto(app.assistant.agents_auto);
        }
    }
    if app.assistant.local_agents && app.assistant.agents_auto {
        ui.label(
            RichText::new(tl!("Agents then run commands, change files and use the web on this computer without asking you."))
                .small()
                .color(t.red),
        );
    }
}

/// The Skills list: every skill and where it comes from, a switch for each, and adding and
/// removing the user's (desktop).
#[cfg(not(target_arch = "wasm32"))]
fn skills_setting(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    use rustgis_assistant::skills::{self, Places, Source};
    ui.collapsing(tl!("Skills"), |ui| {
        // Read once and kept; each frame copies only what the rows show (not the skills' bodies),
        // and the folders are looked up only when something is done.
        let (listed, problems) = app.assistant.skills_list.get_or_insert_with(|| skills::list(&Places::here()));
        let rows: Vec<(String, String, Source, bool)> =
            listed.iter().map(|l| (l.skill.name.clone(), l.skill.description.clone(), l.skill.source.clone(), l.on)).collect();
        let problems = problems.clone();
        let mut done: Option<Result<String, String>> = None;
        for (name, description, source, on) in &rows {
            ui.push_id((name, format!("{source:?}")), |ui| { ui.horizontal(|ui| {
                let mut on = *on;
                if ui.checkbox(&mut on, name).on_hover_text(description).changed() {
                    done = Some(skills::set_source_enabled(&Places::here(), name, source, on).map(|()| String::new()));
                }
                let from = match source {
                    Source::BuiltIn => tl!("Built in"),
                    Source::Plugin(_) => tl!("Plugin"),
                    Source::User(_) => tl!("Yours"),
                    Source::Shared(_) => tl!("Shared"),
                };
                ui.label(RichText::new(from).small().color(t.text_dim));
                if matches!(source, Source::User(_)) && ui.small_button(tl!("Remove")).clicked() {
                    done = Some(skills::remove(&Places::here(), name).map(|()| String::new()));
                }
            }); });
        }
        for p in &problems {
            ui.label(RichText::new(p).small().color(t.red));
        }
        // Wrapped, so the buttons never widen the settings past the pane.
        ui.horizontal_wrapped(|ui| {
            let mut picked = None;
            if ui.button(tl!("Add skill…")).clicked() {
                picked = crate::filedialog::FileDialog::new()
                    .set_title(tl!("Add skill…"))
                    .add_filter(tl!("Skill (SKILL.md or .zip)"), &["md", "zip"])
                    .pick_file();
            }
            if ui.button(tl!("Add folder…")).clicked() {
                picked = crate::filedialog::FileDialog::new().set_title(tl!("Add folder…")).pick_folder();
            }
            if let Some(path) = picked {
                done = Some(
                    skills::add(&Places::here(), &path).map(|names| crate::i18n::fmt("Added: {names}", &[("names", &names.join(", "))])),
                );
            }
            if ui.button(tl!("Open folder")).clicked()
                && let Some(dir) = Places::here().user
            {
                let opened = std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display())).and_then(|()| crate::open_with_system(&dir));
                if let Err(e) = opened {
                    done = Some(Err(e));
                }
            }
            // Skills added to the folder by hand.
            if ui.button(tl!("Refresh")).clicked() {
                done = Some(Ok(String::new()));
            }
        });
        ui.label(
            RichText::new(tl!(
                "Your skills are kept in the skills folder of the app's settings folder. Shared skills (~/.agents/skills) are off until you turn them on."
            ))
            .small()
            .color(t.text_dim),
        );
        if let Some(r) = done {
            app.assistant.skills_message = match r {
                Ok(m) if m.is_empty() => None,
                Ok(m) => Some((true, m)),
                Err(e) => Some((false, e)),
            };
            assistant::sync_skills(app, true);
        }
        if let Some((ok, m)) = &app.assistant.skills_message {
            ui.label(RichText::new(m).small().color(if *ok { t.text_dim } else { t.red }));
        }
    });
}

/// Recovery text is translated; server diagnostics retain their original wording.
fn show_error(ui: &mut Ui, error: &rustgis_assistant::Error, t: &Tokens) {
    ui.label(RichText::new(tl!(error.message())).small().color(t.red));
    if error.recovery == rustgis_assistant::Recovery::Usage
        && crate::widgets::primary_button(ui, tl!("Weekly usage"), true, vec2(160.0, 26.0)).clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(chatgpt::USAGE_URL));
    }
    if error.status.is_some() || error.recovery != rustgis_assistant::Recovery::Other {
        ui.collapsing(tl!("Diagnostics"), |ui| {
            ui.label(&error.detail);
            if let Some(status) = error.status {
                ui.monospace(format!("HTTP {status}"));
            }
            for diagnostic in [&error.code, &error.param, &error.request_id].into_iter().flatten() {
                ui.monospace(diagnostic);
            }
        });
    }
}

/// While a ChatGPT plan is in use: saying so, with a link to its usage settings, and after the
/// first sign-in, a confirmation to dismiss.
fn plan_notice(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if app.assistant.welcome {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new(tl!("You're using your ChatGPT plan")).font(semibold(12.5)));
            ui.label(
                RichText::new(tl!("Eligible requests in RustGIS use your ChatGPT plan. You can manage usage in ChatGPT settings."))
                    .small()
                    .color(t.text_dim),
            );
            if ui.button(tl!("Got it")).clicked() {
                app.assistant.welcome = false;
                save_pref(app, "assistant_chatgpt_welcomed", "on");
            }
        });
    }
    plan_line(ui, t);
}

/// "Using ChatGPT plan · Weekly usage", near the composer and in the settings.
fn plan_line(ui: &mut Ui, t: &Tokens) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("Using ChatGPT plan")).small().color(t.text_dim));
        ui.hyperlink_to(RichText::new(tl!("Weekly usage")).small(), chatgpt::USAGE_URL);
    });
}

#[cfg(target_arch = "wasm32")]
fn account_row(_app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, _cfg: &mut Config) {
    ui.label(RichText::new(tl!("Signing in with ChatGPT works in the desktop app.")).small().color(t.text_dim));
}

/// The input box's least height (two lines), and how tall it grows with text before it scrolls.
const INPUT_HEIGHT: f32 = 44.0;
const INPUT_GROWS: f32 = 160.0;

fn composer(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, busy: bool, ctx: &egui::Context) {
    // Agent threads on this computer, working or waiting for a follow-up.
    #[cfg(not(target_arch = "wasm32"))]
    crate::agentthreads::list(app, ui, t);
    // An action waiting for approval comes first.
    if let Some(Approval { what, why, can_always, review }) = assistant::pending(app) {
        egui::Frame::NONE.fill(t.amber.gamma_multiply(0.15)).corner_radius(6).inner_margin(8).stroke(egui::Stroke::new(1.0, t.amber)).show(
            ui,
            |ui| {
                ui.label(RichText::new(tl!("Allow this action?")).font(semibold(12.5)));
                egui::ScrollArea::vertical().max_height(280.0).id_salt(("assistant_approval", &what, &why)).show(ui, |ui| {
                    if let Some(review) = &review {
                        crate::approvalcard::show(ui, &app.session.registry, review);
                    } else {
                        // Legacy requests have no structured review: keep all their context visible.
                        ui.label(
                            RichText::new(crate::i18n::fmt("The assistant wants to run this, and {why}:", &[("why", tl_raw!(&why))]))
                                .small(),
                        );
                        ui.add(egui::Label::new(RichText::new(&what).monospace().small()).wrap());
                    }
                });
                // Plugin commands, scripts and replacing the project are asked about every time.
                if can_always {
                    ui.checkbox(&mut app.assistant.always, tl!("Allow such actions for this conversation"));
                } else {
                    app.assistant.always = false;
                }
                ui.horizontal(|ui| {
                    let allow = crate::widgets::primary_button(ui, tl!("Allow"), true, vec2(70.0, 26.0)).clicked();
                    let deny = ui.button(tl!("Decline")).clicked();
                    if allow || deny {
                        let always = app.assistant.always;
                        assistant::decide(app, allow, always, ctx);
                    }
                });
            },
        );
        ui.add_space(6.0);
    }
    if app.assistant.config().is_some_and(|c| c.login() && !c.key.is_empty()) {
        plan_line(ui, t);
    }
    let id = egui::Id::unique("assistant_input");
    // Enter sends; Shift+Enter starts a new line.
    let focused = ui.memory(|m| m.has_focus(id));
    crate::assistantpictures::poll(app, ui, focused);
    let mut submit = false;
    if focused && !busy {
        submit = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        // Up and Down step through earlier messages while the box holds one of them (or nothing).
        let shown = app.assistant.history_pos.and_then(|p| app.assistant.history.get(p)).is_some_and(|h| *h == app.assistant.input);
        if app.assistant.input.is_empty() || shown {
            let n = app.assistant.history.len();
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) && n > 0 {
                let p = app.assistant.history_pos.map_or(n - 1, |p| p.saturating_sub(1));
                app.assistant.history_pos = Some(p);
                app.assistant.input = app.assistant.history.get(p).cloned().unwrap_or_default();
            } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
                && let Some(p) = app.assistant.history_pos
            {
                app.assistant.history_pos = (p + 1 < n).then_some(p + 1);
                app.assistant.input = app.assistant.history_pos.and_then(|p| app.assistant.history.get(p)).cloned().unwrap_or_default();
            }
        }
    }
    crate::assistantpictures::chips(app, ui, t);
    // A grip above the box: dragging it up makes the box taller, up to half the window.
    let most = (ui.ctx().content_rect().height() * 0.5).max(INPUT_HEIGHT);
    let (grip, drag) = ui.allocate_exact_size(vec2(ui.available_width(), 8.0), egui::Sense::drag());
    let drag = drag.on_hover_cursor(egui::CursorIcon::ResizeVertical).on_hover_text(tl!("Drag to resize the message box"));
    if drag.dragged() {
        app.assistant.input_height = (app.assistant.input_height.max(INPUT_HEIGHT) - drag.drag_delta().y).clamp(INPUT_HEIGHT, most);
    }
    let color = if drag.hovered() || drag.dragged() { t.text } else { t.text_dim.gamma_multiply(0.6) };
    ui.painter().line_segment([grip.center() - vec2(14.0, 0.0), grip.center() + vec2(14.0, 0.0)], egui::Stroke::new(2.0, color));
    let height = app.assistant.input_height.clamp(INPUT_HEIGHT, most);
    // The box grows with what is typed, past the height set, then scrolls.
    egui::ScrollArea::vertical().id_salt("assistant_input_scroll").max_height(height.max(INPUT_GROWS).min(most)).show(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut app.assistant.input)
                .id(id)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .min_size(vec2(0.0, height))
                .font(regular(13.0))
                .hint_text(tl!("Ask about the map, or tell it what to do…")),
        );
    });
    ui.horizontal(|ui| {
        // Room for the button; the hint gives way in long languages.
        let w = (ui.available_width() - 116.0).max(0.0);
        ui.allocate_ui(vec2(w, 20.0), |ui| {
            ui.add(egui::Label::new(RichText::new(tl!("Enter to send, Shift+Enter for a new line")).small().color(t.text_dim)).truncate());
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if busy {
                if ui.button(tl!("Stop")).clicked() {
                    assistant::stop(app);
                }
            } else {
                // A message is its text, its pictures or both, once its pictures are read.
                let pictures = &app.assistant.pictures;
                let can = (!app.assistant.input.trim().is_empty() || pictures.any()) && !pictures.reading();
                if crate::widgets::primary_button(ui, tl!("Send"), can, vec2(64.0, 26.0)).clicked() {
                    submit = true;
                }
                crate::assistantpictures::button(app, ui);
            }
        });
    });
    // Enter waits for a picture still being read.
    if submit && !busy && !app.assistant.pictures.reading() {
        let text = app.assistant.input.clone();
        send(app, text, ctx);
        ui.memory_mut(|m| m.request_focus(id));
    }
}

fn transcript(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, ctx: &egui::Context) {
    if app.assistant.config().is_none() {
        return;
    }
    if app.assistant.messages().is_empty() {
        ui.label(RichText::new(tl!("Ask a question about your data, or describe what to do with the map. The assistant runs RustGIS commands for you; every change can be undone.")).color(t.text_dim));
        ui.add_space(8.0);
        let mut chosen = None;
        for s in SUGGESTIONS {
            let b = egui::Button::new(RichText::new(tl!(s)).small()).corner_radius(12);
            if ui.add(b).clicked() {
                chosen = Some(tl!(s).to_string());
            }
        }
        if let Some(text) = chosen {
            send(app, text, ctx);
        }
        return;
    }
    for e in app.assistant.messages() {
        match e {
            // A message of pictures alone: its notes say them.
            Entry::User(text) if text.is_empty() => {}
            Entry::User(text) => {
                ui.add_space(6.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    egui::Frame::NONE.fill(t.selection).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.set_max_width(ui.available_width() * 0.85);
                        ui.add(egui::Label::new(RichText::new(text).color(t.text)).halign(Align::LEFT));
                    });
                });
                ui.add_space(4.0);
            }
            Entry::Assistant(text) => {
                markdown(ui, text, t);
                ui.add_space(4.0);
            }
            Entry::Tool { name, summary, result, error } => {
                let (mark, col) = if *error { ("✗", t.red) } else { ("✓", t.green) };
                let head = format!("{mark} {}", summary.chars().take(90).collect::<String>());
                // A header never wraps: it is cut to the room there is. Opened, it shows the whole call.
                let room = (ui.available_width() - ui.spacing().indent - ui.spacing().button_padding.x).max(0.0);
                let head = egui::WidgetText::from(RichText::new(head).monospace().small().color(col)).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    room,
                    egui::TextStyle::Button,
                );
                egui::CollapsingHeader::new(head).id_salt(("assistant_tool", name, summary.as_str(), result.len())).show(ui, |ui| {
                    ui.label(RichText::new(summary).monospace().small());
                    ui.label(RichText::new(result.chars().take(2000).collect::<String>()).monospace().small().color(t.text_dim));
                });
            }
            Entry::Error(e) => {
                show_error(ui, e, t);
            }
            Entry::Note(n) => {
                ui.label(RichText::new(crate::i18n::note(n)).small().italics().color(t.text_dim));
            }
        }
    }
    if app.assistant.waiting() {
        // The reply so far, as it streams in.
        if let Some(text) = app.assistant.preview() {
            markdown(ui, text, t);
            ui.spinner();
            return;
        }
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(tl!("Thinking…")).small().color(t.text_dim));
        });
    }
}

// ---------------------------------------------------------------------------------------------
// A small Markdown renderer: headings, lists, quotes, code blocks, tables (as text), **bold**,
// *italic*, `code` and links (shown as text). Model output is text, never markup to run.

fn markdown(ui: &mut Ui, text: &str, t: &Tokens) {
    let mut code: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(lines) => code_block(ui, &lines.join("\n"), t),
                None => code = Some(Vec::new()),
            }
            continue;
        }
        if let Some(lines) = code.as_mut() {
            lines.push(line);
            continue;
        }
        let trimmed = line.trim_start();
        // A few levels at most: a reply indented further would push its text out of the pane.
        let indent = ((line.len() - trimmed.len()) as f32 * 4.0).min(48.0);
        if trimmed.is_empty() {
            ui.add_space(4.0);
        } else if let Some(h) = ["### ", "## ", "# "].iter().find_map(|p| trimmed.strip_prefix(p)) {
            ui.add_space(2.0);
            ui.label(inline(ui, h, t, 14.0, true));
        } else if let Some(item) = ["- ", "* ", "+ "].iter().find_map(|p| trimmed.strip_prefix(p)) {
            bullet(ui, indent, "•", item, t);
        } else if let Some((num, item)) = numbered(trimmed) {
            bullet(ui, indent, &format!("{num}."), item, t);
        } else if let Some(q) = trimmed.strip_prefix("> ") {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("▎").color(t.border_strong));
                ui.label(inline(ui, q, t, 13.0, false));
            });
        } else if trimmed.starts_with('|') {
            // Table rows as monospace text; the divider row is dropped.
            if !trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                let cells: Vec<&str> = trimmed.trim_matches('|').split('|').map(str::trim).collect();
                ui.label(RichText::new(cells.join("  │  ")).monospace().small());
            }
        } else {
            ui.label(inline(ui, trimmed, t, 13.0, false));
        }
    }
    if let Some(lines) = code {
        code_block(ui, &lines.join("\n"), t);
    }
}

fn numbered(s: &str) -> Option<(&str, &str)> {
    let (num, rest) = s.split_once(". ")?;
    (!num.is_empty() && num.len() <= 3 && num.chars().all(|c| c.is_ascii_digit())).then_some((num, rest))
}

fn bullet(ui: &mut Ui, indent: f32, mark: &str, item: &str, t: &Tokens) {
    ui.horizontal_top(|ui| {
        ui.add_space(4.0 + indent);
        ui.label(RichText::new(mark).color(t.text_dim));
        ui.label(inline(ui, item, t, 13.0, false));
    });
}

fn code_block(ui: &mut Ui, code: &str, t: &Tokens) {
    egui::Frame::NONE.fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(4).inner_margin(6).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(code).monospace().small());
    });
}

/// A line with **bold**, *italic*, `code` and [links](url) (as their text) laid out.
fn inline(ui: &Ui, text: &str, t: &Tokens, size: f32, heading: bool) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let base = |bold: bool, italic: bool, code: bool| {
        let font = if code {
            egui::FontId::monospace(size - 1.0)
        } else if bold || heading {
            semibold(size)
        } else {
            regular(size)
        };
        TextFormat {
            font_id: font,
            color: if code { t.accent_text } else { t.text },
            italics: italic,
            background: if code { t.input } else { egui::Color32::TRANSPARENT },
            ..Default::default()
        }
    };
    let (mut bold, mut italic) = (false, false);
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool| {
        if !buf.is_empty() {
            job.append(buf, 0.0, base(bold, italic, false));
            buf.clear();
        }
    };
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            flush(&mut job, &mut buf, bold, italic);
            bold = !bold;
            rest = r;
        } else if c == '`'
            && let Some(end) = rest.get(1..).and_then(|r| r.find('`'))
        {
            flush(&mut job, &mut buf, bold, italic);
            job.append(rest.get(1..1 + end).unwrap_or(""), 0.0, base(bold, italic, true));
            rest = rest.get(end + 2..).unwrap_or("");
        } else if c == '*' && rest.get(1..).is_some_and(|r| !r.starts_with(' ')) {
            flush(&mut job, &mut buf, bold, italic);
            italic = !italic;
            rest = rest.get(1..).unwrap_or("");
        } else if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest.get(close..).and_then(|r| r.find(')'))
        {
            buf.push_str(rest.get(1..close).unwrap_or(""));
            rest = rest.get(close + end + 1..).unwrap_or("");
        } else {
            buf.push(c);
            rest = rest.get(c.len_utf8()..).unwrap_or("");
        }
    }
    flush(&mut job, &mut buf, bold, italic);
    job
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Slot;
    use crate::assistant::{
        AssistantState, Chunks, apply_config, dispatch, ensure_loaded, list_models, poll, save_config, take, toggle_settings,
    };
    #[cfg(not(target_arch = "wasm32"))]
    use crate::assistant::{Reply, SignInResult, poll_keys, poll_sign_in, signed_in};
    use rustgis_assistant::{Agent, HttpRequest, HttpResponse, Step};

    #[cfg(not(target_arch = "wasm32"))]
    type KeyPrefs = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>;

    /// An Assistant with isolated preferences and a fake keychain, never the OS store.
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_app(
        legacy: Option<&str>,
        store: Option<std::sync::Arc<dyn rustgis_engine::data::secrets::SecretStore>>,
    ) -> (RustGisApp, KeyPrefs) {
        keys_app_remember(legacy, store, true)
    }

    /// Builds either the opt-in or opt-out startup path with isolated preference storage.
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_app_remember(
        legacy: Option<&str>,
        store: Option<std::sync::Arc<dyn rustgis_engine::data::secrets::SecretStore>>,
        remember: bool,
    ) -> (RustGisApp, KeyPrefs) {
        use std::{
            collections::BTreeMap,
            sync::{Arc, Mutex},
        };
        let prefs = Arc::new(Mutex::new(BTreeMap::from([
            (provider::PROVIDER_PREF.into(), "custom".into()),
            (provider::REMEMBER_KEY_PREF.into(), provider::remember_key_value(remember).into()),
        ])));
        if let Some(key) = legacy {
            prefs.lock().unwrap().insert("assistant_key_custom".into(), key.into());
        }
        let mut app = RustGisApp::with_sample();
        let read = prefs.clone();
        app.services.load_pref = Some(Box::new(move |key| read.lock().unwrap().get(key).cloned()));
        let write = prefs.clone();
        app.services.save_pref = Some(Box::new(move |key, value| {
            write.lock().unwrap().insert(key.into(), value.into());
        }));
        app.services.secrets = store;
        ensure_loaded(&mut app, &egui::Context::default());
        (app, prefs)
    }

    /// Waits for the fake keychain and applies its preference cleanup on the UI side.
    #[cfg(not(target_arch = "wasm32"))]
    fn settle_keys(app: &mut RustGisApp) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            poll_keys(app, &egui::Context::default());
            let provider = app.assistant.agent.as_ref().map_or("custom", |a| a.config.provider.as_str());
            if !matches!(app.assistant.keys.status(provider), Some(crate::assistantkeys::Status::Pending { .. })) {
                return;
            }
            assert!(std::time::Instant::now() < end, "fake keychain timed out");
            std::thread::sleep(std::time::Duration::from_millis(1)); // test-wait: legacy polling for the fake keychain
        }
    }

    /// Only successful migrations clear legacy preferences; failures keep them usable and visible.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_migrate_without_losing_a_legacy_key() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        let (mut app, prefs) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
        settle_keys(&mut app);
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("synthetic-legacy")));
        assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some(""));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-legacy");
        assert!(key_storage_note(&app, "custom").contains("system keychain"));
        let (mut failed, prefs) =
            keys_app(Some("synthetic-legacy"), Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))));
        settle_keys(&mut failed);
        assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some("synthetic-legacy"));
        assert_eq!(failed.assistant.agent.as_ref().unwrap().config.key, "synthetic-legacy");
        let note = key_storage_note(&failed, "custom");
        assert!(note.contains("unencrypted") && note.contains("refused"));
        assert!(!note.contains("synthetic-legacy"));
    }

    /// Desktop without a store never writes a new plaintext key; opting out also clears legacy plaintext.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_have_no_new_plaintext_fallback() {
        let (mut app, prefs) = keys_app(None, None);
        let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
        cfg.key = "synthetic-new".into();
        app.assistant.keys.edit("custom", &cfg.key);
        apply_config(&mut app, cfg.clone());
        save_config(&mut app, &cfg, &egui::Context::default());
        assert!(!prefs.lock().unwrap().contains_key("assistant_key_custom"));
        let note = key_storage_note(&app, "custom");
        assert!(note.contains("unavailable") && note.contains("session only"));
        assert!(!note.contains("synthetic-new"));
        let (mut legacy, prefs) = keys_app(Some("synthetic-legacy"), None);
        legacy.assistant.remember_key = false;
        let cfg = legacy.assistant.agent.as_ref().unwrap().config.clone();
        legacy.assistant.keys.edit("custom", &cfg.key);
        save_config(&mut legacy, &cfg, &egui::Context::default());
        assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some(""));
    }

    /// Remember kept on where there is no store at all: no saved key is said to be unreadable.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn remember_with_no_store_says_no_key_was_lost() {
        let (mut app, _) = keys_app(None, None);
        settle_keys(&mut app);
        let note = key_storage_note(&app, "custom");
        assert!(note.contains("unavailable") && note.contains("for this session only"), "{note}");
        assert!(!note.contains("could not be read"), "{note}");
        // Nor is a key cleared there said to be still in a keychain.
        let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
        cfg.key.clear();
        app.assistant.keys.edit("custom", &cfg.key);
        apply_config(&mut app, cfg.clone());
        save_config(&mut app, &cfg, &egui::Context::default());
        settle_keys(&mut app);
        let note = key_storage_note(&app, "custom");
        assert!(note.contains("for this session only") && !note.contains("deleted"), "{note}");
    }

    /// A failed new-key write leaves the key in the session and never echoes backend payloads.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_report_failed_writes_without_leaking_the_key() {
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError};
        for error in [SecretError::Denied, SecretError::Failed("synthetic-new".into())] {
            let (mut app, prefs) = keys_app(None, Some(std::sync::Arc::new(MemorySecrets::failing(error))));
            settle_keys(&mut app);
            assert!(key_storage_note(&app, "custom").contains("could not be read"));
            let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
            cfg.key = "synthetic-new".into();
            app.assistant.keys.edit("custom", &cfg.key);
            apply_config(&mut app, cfg.clone());
            save_config(&mut app, &cfg, &egui::Context::default());
            settle_keys(&mut app);
            assert!(!prefs.lock().unwrap().contains_key("assistant_key_custom"));
            assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-new");
            let note = key_storage_note(&app, "custom");
            assert!(note.contains("session only") && !note.contains("synthetic-new"));
        }
    }

    /// A failed replacement is session-only even when a different legacy key still needs migration.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_distinguish_a_failed_replacement_from_legacy_migration() {
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError, SecretStore};
        let failing: std::sync::Arc<dyn SecretStore> =
            std::sync::Arc::new(MemorySecrets::failing(SecretError::Failed("synthetic-backend-payload".into())));
        for store in [None, Some(failing)] {
            let (mut app, prefs) = keys_app(Some("synthetic-legacy"), store);
            settle_keys(&mut app);
            let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
            cfg.key = "synthetic-replacement".into();
            app.assistant.keys.edit("custom", &cfg.key);
            apply_config(&mut app, cfg.clone());
            save_config(&mut app, &cfg, &egui::Context::default());
            settle_keys(&mut app);
            assert_eq!(prefs.lock().unwrap().get("assistant_key_custom").map(String::as_str), Some("synthetic-legacy"));
            assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-replacement");
            let note = key_storage_note(&app, "custom");
            assert!(note.contains("session only; it has not been remembered") && note.contains("unencrypted"));
            for secret in ["synthetic-legacy", "synthetic-replacement", "synthetic-backend-payload"] {
                assert!(!note.contains(secret));
            }
        }
    }

    /// Requests wait for the saved key instead of sending with a temporary empty/environment value.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_wait_before_contacting_a_provider() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        store.save("assistant/custom", &Secret::new("synthetic-stored")).unwrap();
        let (mut app, _) = keys_app(None, Some(store));
        assert!(key_loading(&app));
        let cfg = app.assistant.agent.as_ref().unwrap().config.clone();
        let ctx = egui::Context::default();
        list_models(&mut app, &cfg, &ctx);
        send(&mut app, "hello".into(), &ctx);
        assert!(app.assistant.catalog_pending && app.assistant.models_slot.is_none());
        assert!(app.assistant.inflight.is_none());
        assert_eq!(app.assistant.queued.as_deref(), Some("hello"));
        settle_keys(&mut app);
        assert!(!key_loading(&app));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, "synthetic-stored");
    }

    /// Startup waits for the saved key before opening settings or restoring a folded provider.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_settings_follow_the_initial_key_read() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretError, SecretStore};
        for saved in [false, true] {
            for collapsed in [false, true] {
                let store = std::sync::Arc::new(MemorySecrets::new());
                if saved {
                    store.save("assistant/openai", &Secret::new("synthetic-stored")).unwrap();
                }
                let (mut app, prefs) = keys_app(None, None);
                prefs.lock().unwrap().insert(provider::PROVIDER_PREF.into(), "openai".into());
                prefs.lock().unwrap().insert("assistant_provider_closed".into(), if collapsed { "on" } else { "" }.into());
                app.assistant = AssistantState::default();
                app.services.secrets = Some(store);
                let ctx = egui::Context::default();
                app.assistant.keys.load("openai", "", None, true, app.services.secrets.clone(), &ctx);
                ensure_loaded(&mut app, &ctx);
                assert!(key_loading(&app));
                assert!(!app.assistant.settings);
                settle_keys(&mut app);
                assert_eq!(app.assistant.settings, !saved);
                assert_eq!(app.assistant.provider_closed, saved && collapsed);
            }
        }
        let (mut failed, _) = keys_app(None, Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))));
        failed.assistant.agent.as_mut().unwrap().config.model.clear();
        settle_keys(&mut failed);
        assert!(failed.assistant.settings && !failed.assistant.provider_closed);
    }

    /// A user's open or close choice wins even when it returns to the pending default.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_settings_keep_user_choices_during_the_initial_key_read() {
        use rustgis_engine::data::secrets::MemorySecrets;
        for open in [false, true] {
            let (mut app, _) = keys_app(None, Some(std::sync::Arc::new(MemorySecrets::new())));
            assert!(key_loading(&app));
            app.assistant.agent.as_mut().unwrap().config.model = if open { "synthetic-model" } else { "" }.into();
            toggle_settings(&mut app);
            if !open {
                toggle_settings(&mut app);
            }
            settle_keys(&mut app);
            assert_eq!(app.assistant.settings, open);
        }
    }

    /// Clearing or unticking Remember deletes the key while keeping an unticked key usable this session.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_are_deleted_when_cleared_or_not_remembered() {
        use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
        for clear in [false, true] {
            let store = std::sync::Arc::new(MemorySecrets::new());
            let (mut app, _) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
            settle_keys(&mut app);
            let mut cfg = app.assistant.agent.as_ref().unwrap().config.clone();
            if clear {
                cfg.key.clear();
            } else {
                app.assistant.remember_key = false;
            }
            app.assistant.keys.edit("custom", &cfg.key);
            apply_config(&mut app, cfg.clone());
            save_config(&mut app, &cfg, &egui::Context::default());
            settle_keys(&mut app);
            assert_eq!(store.load("assistant/custom").unwrap(), None);
            assert_eq!(app.assistant.agent.as_ref().unwrap().config.key, if clear { "" } else { "synthetic-legacy" });
        }
    }

    /// The shared settings harness installs the real fonts before drawing the form.
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(700.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                let ctx = ui.ctx().clone();
                poll(app, &ctx);
                settings(app, ui, &Tokens::light());
            },
            app,
        )
    }

    /// Render the real local-agent settings without polling threads, preserving the interval
    /// between a checkbox callback and the next poll while the fake ACP agent replies.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn local_agents_harness(mut app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        app.assistant.local_agents = true;
        app.assistant.agents_auto = true;
        app.assistant.agents_found = Some((Vec::new(), Vec::new()));
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(700.0, 350.0)).build_ui_state(
            move |ui, app| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                local_agents_setting(app, ui, &Tokens::light());
            },
            app,
        )
    }

    /// The actual Remember checkbox goes through the deferred save path and deletes the stored key.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_remember_checkbox_forgets_the_key() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        let (mut app, prefs) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
        settle_keys(&mut app);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("Remember the key on this computer").click();
        h.run_steps(4);
        settle_keys(h.state_mut());
        assert!(!h.state().assistant.remember_key);
        assert_eq!(store.load("assistant/custom").unwrap(), None);
        assert_eq!(prefs.lock().unwrap().get(provider::REMEMBER_KEY_PREF).map(String::as_str), Some("off"));
    }

    /// Ticking Remember after an off startup loads the existing key instead of deleting it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_remember_checkbox_loads_an_unread_key() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        store.save("assistant/custom", &Secret::new("synthetic-stored")).unwrap();
        let (app, prefs) = keys_app_remember(None, Some(store.clone()), false);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("Remember the key on this computer").click();
        h.run_steps(4);
        settle_keys(h.state_mut());
        assert!(h.state().assistant.remember_key);
        assert_eq!(h.state().assistant.agent.as_ref().unwrap().config.key, "synthetic-stored");
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("synthetic-stored")));
        assert_eq!(prefs.lock().unwrap().get(provider::REMEMBER_KEY_PREF).map(String::as_str), Some("on"));
    }

    /// Clearing the focused password field deletes its saved key before the field loses focus.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_clear_without_waiting_for_focus_loss() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretStore};
        let store = std::sync::Arc::new(MemorySecrets::new());
        let (mut app, _) = keys_app(Some("synthetic-legacy"), Some(store.clone()));
        settle_keys(&mut app);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("API key").focus();
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.key_press(egui::Key::Backspace);
        h.run_steps(3);
        assert!(h.get_by_label("API key").is_focused());
        assert!(h.state().assistant.agent.as_ref().unwrap().config.key.is_empty());
        settle_keys(h.state_mut());
        assert_eq!(store.load("assistant/custom").unwrap(), None);
    }

    /// Session-only typing waits for focus loss instead of deleting a key on each keystroke.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_session_key_typing_defers_storage() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::secrets::{MemorySecrets, SecretError};
        let (app, prefs) = keys_app_remember(None, Some(std::sync::Arc::new(MemorySecrets::failing(SecretError::Denied))), false);
        let mut h = keys_harness(app);
        h.run_steps(3);
        h.get_by_label("API key").focus();
        h.run_steps(2);
        prefs.lock().unwrap().remove("assistant_model_custom");
        for text in ["synthetic", "-session"] {
            h.get_by_label("API key").type_text(text);
            h.run_steps(3);
            assert!(h.get_by_label("API key").is_focused());
            assert!(matches!(h.state().assistant.keys.status("custom"), Some(crate::assistantkeys::Status::Session)));
            assert!(!prefs.lock().unwrap().contains_key("assistant_model_custom"));
        }
        assert_eq!(h.state().assistant.agent.as_ref().unwrap().config.key, "synthetic-session");
        h.get_by_label("Remember the key on this computer").focus();
        h.run_steps(3);
        settle_keys(h.state_mut());
        assert!(prefs.lock().unwrap().contains_key("assistant_model_custom"));
        assert!(matches!(h.state().assistant.keys.status("custom"), Some(crate::assistantkeys::Status::Failed { deleting: true, .. })));
    }

    /// Startup migrates keys for unselected providers without opening the Assistant or contacting models.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn assistant_keys_migrate_all_legacy_providers_at_startup() {
        use rustgis_engine::data::secrets::{MemorySecrets, Secret, SecretStore};
        use std::{
            collections::BTreeMap,
            sync::{Arc, Mutex},
        };
        let prefs = Arc::new(Mutex::new(BTreeMap::from([
            (provider::REMEMBER_KEY_PREF.to_string(), "on".to_string()),
            ("assistant_key_custom".into(), "first".into()),
            ("assistant_key_anthropic".into(), "second".into()),
        ])));
        let store = Arc::new(MemorySecrets::new());
        let mut app = RustGisApp::with_sample();
        let read = prefs.clone();
        app.services.load_pref = Some(Box::new(move |key| read.lock().unwrap().get(key).cloned()));
        let write = prefs.clone();
        app.services.save_pref = Some(Box::new(move |key, value| {
            write.lock().unwrap().insert(key.into(), value.into());
        }));
        app.services.secrets = Some(store.clone());
        let ctx = egui::Context::default();
        poll_keys(&mut app, &ctx);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while ["custom", "anthropic"]
            .iter()
            .any(|p| matches!(app.assistant.keys.status(p), Some(crate::assistantkeys::Status::Pending { .. })))
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(1)); // test-wait: legacy polling for migrated provider keys
            poll_keys(&mut app, &ctx);
        }
        assert!(app.assistant.agent.is_none());
        assert_eq!(store.load("assistant/custom").unwrap(), Some(Secret::new("first")));
        assert_eq!(store.load("assistant/anthropic").unwrap(), Some(Secret::new("second")));
        for p in ["custom", "anthropic"] {
            assert_eq!(prefs.lock().unwrap().get(&format!("assistant_key_{p}")).map(String::as_str), Some(""));
        }
    }

    /// A signed-out ChatGPT settings pane for tests that never contact a provider.
    fn plan_app() -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        let mut cfg = Config::from_env("chatgpt");
        cfg.key.clear();
        cfg.account.clear();
        cfg.client_id.clear();
        app.assistant.agent = Some(Agent::new(cfg, &app.session));
        app.assistant.loaded = true;
        app.run("ui.pane.assistant", serde_json::Value::Null).unwrap();
        app.assistant.settings = true;
        app
    }

    /// Turned off as its extension with a run under way, through the app's own frame: the run
    /// stops, the conversation and the message waiting are kept and said, and nothing is sent
    /// until it is on again.
    #[test]
    fn turning_the_extension_off_stops_a_run_keeps_the_conversation_and_sends_nothing() {
        let mut app = RustGisApp::with_sample();
        let sent = std::sync::Arc::new(std::sync::Mutex::new(0));
        let count = sent.clone();
        app.services.http = Some(Box::new(move |_, _| *count.lock().unwrap() += 1));
        let mut config = Config::from_env("custom");
        config.base_url = "https://example.test/v1".into();
        config.model = "test".into();
        let mut agent = Agent::new(config, &app.session);
        let request = agent.ask("Zoom to Tokyo", &app.session);
        assert!(agent.busy());
        app.assistant.inflight = Some((agent.generation(), crate::Slot::default()));
        app.assistant.agent = Some(agent);
        app.assistant.loaded = true;
        let ctx = egui::Context::default();
        let frame = |app: &mut RustGisApp| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
            output.textures_delta.clear();
        };
        frame(&mut app);
        let said = |app: &RustGisApp| -> Vec<String> {
            app.session.notices.iter().filter(|n| n.text.contains("AI Assistant")).map(|n| n.text.clone()).collect()
        };
        assert_eq!(said(&app), Vec::<String>::new());
        // A message asked for and not sent yet.
        app.run("ui.pane.assistant", serde_json::json!({"ask": "Label the cities"})).unwrap();

        app.run("extension.disable", serde_json::json!({"id": "assistant"})).unwrap();
        // A step that follows the command turning it off (an approved one of its own) isn't sent.
        crate::assistant::follow(&mut app, request, &ctx);
        // A model list due waits too.
        app.assistant.catalog_pending = true;
        frame(&mut app);
        frame(&mut app);
        let agent = app.assistant.agent.as_ref().unwrap();
        assert!(!agent.busy());
        let kept = agent.transcript.len();
        assert!(kept > 0);
        assert!(app.assistant.inflight.is_none() && app.assistant.queued.is_none());
        assert_eq!(app.assistant.input, "Label the cities");
        assert!(app.ui.right_open.is_empty());
        let notices = said(&app);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("running request was stopped") && notices[0].contains("conversation is kept"), "{notices:?}");
        assert_eq!(*sent.lock().unwrap(), 0);
        // Pictures picked before, landing now, are counted in one notice rather than attached; a
        // cancelled pick isn't one.
        for name in ["map.png", "sketch.png", ""] {
            app.open_picked(&format!("{}{name}", crate::ASSISTANT_PICK), &[0]).unwrap();
        }
        frame(&mut app);
        let late: Vec<String> = said(&app).into_iter().filter(|n| n.contains("Pictures not attached")).collect();
        assert_eq!(late.len(), 1, "{late:?}");
        assert!(late[0].ends_with("not attached: 2."), "{late:?}");
        assert!(!app.assistant.pictures.any() && !app.assistant.pictures.reading());

        app.run("extension.enable", serde_json::json!({"id": "assistant"})).unwrap();
        frame(&mut app);
        assert_eq!(app.assistant.agent.as_ref().unwrap().transcript.len(), kept);
        assert_eq!(*sent.lock().unwrap(), 1);
    }

    /// Clicking Stop drops the old UI backoff deadline and streamed chunks before another run.
    #[test]
    fn stop_button_clears_retry_deadline_and_chunks() {
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        let mut config = Config::from_env("custom");
        config.model = "test".into();
        let mut agent = Agent::new(config, &app.session);
        assert!(matches!(agent.ask("hello", &app.session), Step::Send(_)));
        let response = Ok(HttpResponse { status: 503, body: b"{}".to_vec(), headers: vec![("retry-after".into(), "0".into())] });
        assert!(matches!(agent.receive_result(agent.generation(), response, &mut app.session), Step::Retry(_)));
        let generation = agent.generation();
        app.assistant.agent = Some(agent);
        app.assistant.retry = Some((generation, web_time::Instant::now()));
        app.assistant.chunks = Some((generation, Chunks::default()));
        app.assistant.inflight = Some((generation, Slot::default()));
        // Render just the composer: no polling or network dispatch is needed to press Stop.
        let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 160.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                let busy = app.assistant.agent.as_ref().is_some_and(Agent::busy);
                composer(app, ui, &Tokens::light(), busy, &ctx);
            },
            app,
        );
        h.run();
        h.get_by_label("Stop").click();
        h.step();
        assert!(h.state().assistant.retry.is_none());
        assert!(h.state().assistant.chunks.is_none());
        assert!(h.state().assistant.inflight.is_none());
        assert!(!h.state().assistant.agent.as_ref().unwrap().busy());
    }

    /// A message sent while the assistant is busy is refused with a notice and leaves the draft,
    /// the conversation and the provider alone; an idle assistant raises no such notice.
    #[test]
    fn send_while_busy_is_refused_with_a_notice() {
        const BUSY: &str = "The assistant is busy";
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let mut config = Config::from_env("custom");
        config.model = "test".into();
        app.assistant.agent = Some(Agent::new(config.clone(), &app.session));
        app.assistant.loaded = true;
        app.session.notices.clear();
        send(&mut app, "   ".into(), &ctx);
        assert!(app.session.notices.iter().all(|n| !n.text.contains(BUSY)));

        let mut agent = Agent::new(config, &app.session);
        assert!(matches!(agent.ask("hello", &app.session), Step::Send(_)));
        let entries = agent.transcript.len();
        app.assistant.agent = Some(agent);
        app.assistant.input = "second thoughts".into();
        send(&mut app, "second thoughts".into(), &ctx);
        assert_eq!(app.session.notices.iter().filter(|n| n.text.contains(BUSY)).count(), 1);
        assert_eq!(app.assistant.input, "second thoughts");
        assert!(app.assistant.queued.is_none());
        let agent = app.assistant.agent.as_ref().unwrap();
        assert_eq!(agent.transcript.len(), entries);
        assert!(app.assistant.history.iter().all(|h| h != "second thoughts"));
        // A suggestion, or a command's message, refused while busy is put in the box to send again.
        app.assistant.input.clear();
        send(&mut app, "a suggestion".into(), &ctx);
        assert_eq!(app.assistant.input, "a suggestion");
    }

    /// A sent message clears the draft only when the draft still holds it: text typed while a
    /// message waited (for the key, say) stays in the box.
    #[test]
    fn sending_clears_only_the_draft_it_sent() {
        let ctx = egui::Context::default();
        let mut app = RustGisApp::with_sample();
        let mut config = Config::from_env("custom");
        config.model = "test".into();
        app.assistant.loaded = true;
        // The request goes nowhere: the test is about the draft, not the reply.
        app.services.http = Some(Box::new(|_, _| {}));
        for (draft, sent, kept) in [("newer draft", "waited", "newer draft"), ("  waited ", "waited", "")] {
            app.assistant.agent = Some(Agent::new(config.clone(), &app.session));
            app.assistant.input = draft.into();
            send(&mut app, sent.into(), &ctx);
            assert_eq!(app.assistant.input, kept);
            let agent = app.assistant.agent.as_ref().unwrap();
            assert!(matches!(agent.transcript.first(), Some(Entry::User(t)) if t == sent));
        }
    }

    /// Signing out puts a waiting message back into the box: as the draft, or above newer typing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn signing_out_returns_a_waiting_message_to_the_box() {
        let mut app = RustGisApp::with_sample();
        let mut cfg = Config::from_env("custom");
        for (draft, kept) in [("typed since", "waited\ntyped since"), ("", "waited"), ("waited", "waited")] {
            app.assistant.queued = Some("waited".into());
            app.assistant.input = draft.into();
            assistant::forget_sign_in(&mut app, &mut cfg);
            assert!(app.assistant.queued.is_none());
            assert_eq!(app.assistant.input, kept);
        }
    }

    /// A new conversation doesn't send a message still waiting into the next one: it goes back to
    /// the box (a suggestion's too, which never was the draft).
    #[test]
    fn new_conversation_returns_a_waiting_message_to_the_box() {
        let mut app = RustGisApp::with_sample();
        app.assistant.agent = Some(Agent::new(Config::from_env("custom"), &app.session));
        for draft in ["from before", ""] {
            app.assistant.queued = Some("from before".into());
            app.assistant.input = draft.into();
            assistant::new_conversation(&mut app);
            assert!(app.assistant.queued.is_none());
            assert_eq!(app.assistant.input, "from before");
        }
        // A command's message doesn't push one still waiting out: that one goes to the box.
        app.assistant.queued = Some("waiting".into());
        app.assistant.input.clear();
        app.run("ui.pane.assistant", serde_json::json!({"ask": "from a command"})).unwrap();
        assert_eq!((app.assistant.queued.as_deref(), app.assistant.input.as_str()), (Some("from a command"), "waiting"));
        // Nor into a peer's project that replaces this one.
        app.assistant.queued = Some("a suggestion".into());
        app.assistant.input.clear();
        assistant::peer_replaced(&mut app);
        assert!(app.assistant.queued.is_none());
        assert_eq!(app.assistant.input, "a suggestion");
    }

    /// Real pending reviews keep their reach visible while exact requests wait under Details.
    #[test]
    fn compact_approval_details_preserve_frozen_request_and_defaults() {
        use egui_kittest::kittest::Queryable;
        use serde_json::json;

        for (tool, args, expected) in [
            (
                "run",
                json!({"script": concat!(
                    "layer.visibility {\"layer\":\"Countries\",\"visible\":false}\n",
                    "layer.visibility {\"visible\":false}\n",
                    "map.addTiles {\"name\":\"Synthetic tiles\",\"url\":\"https://tiles.invalid/{z}/{x}/{y}.png\"}"
                )}),
                vec!["3 project edits", "line 3 (map.addTiles)", "World / Countries", "https://tiles.invalid/{z}/{x}/{y}.png"],
            ),
            (
                "run_command",
                json!({"command":"layer.addSql","params":{"sql":"FROM t","database":"/synthetic/views.duckdb"}}),
                vec!["views in the database /synthetic/views.duckdb", "Destinations"],
            ),
        ] {
            let mut app = RustGisApp::with_sample();
            app.session.run("map.basemap", &json!({"name":"ocean"})).unwrap();
            app.session.run("layer.select", &json!({"layer":"Cities"})).unwrap();
            let config = Config {
                provider: "custom".into(),
                base_url: "https://provider.invalid/v1".into(),
                model: "synthetic".into(),
                key: String::new(),
                account: String::new(),
                client_id: String::new(),
                session: String::new(),
                reasoning: None,
                tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
            };
            let mut agent = Agent::new(config, &app.session);
            assert!(matches!(agent.ask("Review only", &app.session), Step::Send(_)));
            let body = json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{
                "id":"compact-card","type":"function","function":{"name":tool,"arguments":args.to_string()}
            }]},"finish_reason":"tool_calls"}]})
            .to_string()
            .into_bytes();
            assert_eq!(
                agent.receive(agent.generation(), Ok(HttpResponse { status: 200, body, headers: vec![] }), &mut app.session),
                Step::Confirm
            );
            let exact = agent.pending_review().unwrap().what.clone();
            let can_always = agent.pending(&app.session).unwrap().2;
            let project = serde_json::to_value(&app.session.project).unwrap();
            app.assistant.agent = Some(agent);
            let mut fonts = false;
            let mut h = egui_kittest::Harness::builder().with_size(vec2(680.0, 600.0)).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    if !fonts {
                        crate::theme::install_fonts(ui.ctx(), &[], None);
                        fonts = true;
                        ui.ctx().request_repaint();
                        return;
                    }
                    let ctx = ui.ctx().clone();
                    composer(app, ui, &Tokens::light(), true, &ctx);
                },
                app,
            );
            h.run();
            assert!(h.query_by_label(&exact).is_none(), "raw vehicle starts collapsed");
            for text in expected {
                assert!(h.query_by_label_contains(text).is_some(), "missing overview context: {text}");
            }
            assert!(h.query_by_label_contains("Starting active layer: Cities; selected layers: [Cities]").is_some());
            assert_eq!(h.query_by_label("Allow such actions for this conversation").is_some(), can_always);
            h.get_by_label("Details").click();
            h.run();
            h.get_by_label(&exact);
            h.get_by_label("Allow");
            h.get_by_label("Decline");
            h.get_by_label("Details").click();
            h.run();
            assert!(h.query_by_label(&exact).is_none());
            assert_eq!(h.state().assistant.agent.as_ref().unwrap().pending_review().unwrap().what, exact);
            assert_eq!(serde_json::to_value(&h.state().session.project).unwrap(), project);
            assert!(!h.state().assistant.always);
        }
    }

    #[test]
    fn plan_settings_order_and_signed_out_model() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut h = egui_kittest::Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            plan_app(),
        );
        h.run();
        let provider = h.get_by_label("Provider").rect().min.y;
        let account = h.get_by_label("Account").rect().min.y;
        let model = h.get_by_label("Model").rect().min.y;
        assert!(provider < account && account < model);
        assert!(h.query_by_label("Address").is_none());
        assert!(h.get_by_label("Sign in to choose a model.").accesskit_node().is_disabled());
        let mut cfg = Config::from_env("custom");
        cfg.model = "local-model".into();
        h.state_mut().assistant.agent.as_mut().unwrap().config = cfg;
        h.run();
        assert!(h.query_by_label("Address").is_some());
        assert!(h.query_by_label("API key").is_some());
    }

    /// The AI provider section folds away once set up, and the choice is saved.
    #[test]
    fn provider_section_collapses_and_is_remembered() {
        use egui_kittest::kittest::Queryable;
        use std::sync::{Arc, Mutex};
        let mut app = plan_app();
        let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let log = saved.clone();
        app.services.save_pref = Some(Box::new(move |k, v| log.lock().unwrap().push((k.to_string(), v.to_string()))));
        let mut h = egui_kittest::Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Provider").is_some());
        h.get_by_label("AI provider").click();
        h.run();
        assert!(h.query_by_label("Provider").is_none());
        assert!(h.state().assistant.provider_closed);
        assert!(saved.lock().unwrap().contains(&("assistant_provider_closed".into(), "on".into())));
        h.get_by_label("AI provider").click();
        h.run();
        assert!(h.query_by_label("Provider").is_some());
        assert!(saved.lock().unwrap().contains(&("assistant_provider_closed".into(), String::new())));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn waiting_controls_reopen_copy_and_cancel() {
        use egui_kittest::kittest::Queryable;
        let url = "https://auth.openai.com/synthetic-sign-in";
        let mut h = egui_kittest::Harness::builder().with_size(vec2(260.0, 220.0)).build_ui_state(
            |ui, actions: &mut (bool, bool)| {
                let (reopen, cancel) = waiting_for_browser(ui, &Tokens::light(), url);
                actions.0 |= reopen;
                actions.1 |= cancel;
            },
            (false, false),
        );
        h.step();
        for label in ["Waiting for the browser…", "Open browser again", "Copy sign-in link", "Cancel"] {
            let rect = h.get_by_label(label).rect();
            assert!(rect.max.x <= 260.0 && rect.max.y <= 220.0, "{label}: {rect:?}");
        }
        h.get_by_label("Open browser again").click();
        h.step();
        assert_eq!(*h.state(), (true, false));
        h.get_by_label("Copy sign-in link").click();
        h.step();
        assert!(h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == url)));
        h.get_by_label("Cancel").click();
        h.step();
        assert_eq!(*h.state(), (true, true));
    }

    #[test]
    fn tall_settings_keep_transcript_above_composer() {
        use egui_kittest::kittest::Queryable;
        // A ChatGPT plan and a provider with a key: the settings scroll above the composer either way.
        for (plan, width, height) in [(true, 960.0, 700.0), (true, 800.0, 600.0), (false, 800.0, 420.0)] {
            let mut app = if plan {
                plan_app()
            } else {
                let mut app = RustGisApp::with_sample();
                app.assistant.agent = Some(Agent::new(Config::from_env("custom"), &app.session));
                app.assistant.loaded = true;
                app.run("ui.pane.assistant", serde_json::Value::Null).unwrap();
                app.assistant.settings = true;
                app
            };
            app.assistant.login_error = Some(rustgis_assistant::Error::new(rustgis_assistant::Recovery::Auth, ""));
            let mut h = egui_kittest::Harness::builder().with_size(vec2(width, height)).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.run();
            assert!(h.get_by_label("Send").rect().max.y <= height - 26.0);
            let input_top = h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect().min.y;
            for shape in &h.output().shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape
                    && SUGGESTIONS.contains(&text.galley.job.text.as_str())
                {
                    assert!(shape.clip_rect.max.y <= input_top, "suggestions overlap the composer");
                }
            }
        }
    }

    /// Runs the test `name` again in a child process whose settings folder is a fresh temporary
    /// one, so ChatGPT credentials and lock files never touch the developer's own (the
    /// environment of this process is left alone). Gives whether this is the child, which runs
    /// the test's body.
    #[cfg(not(target_arch = "wasm32"))]
    fn in_isolated_settings(name: &str) -> bool {
        if std::env::var_os("RUSTGIS_TEST_ISOLATED").is_some() {
            return true;
        }
        let short = name.rsplit("::").next().unwrap_or(name);
        let dir = std::env::temp_dir().join(format!("rustgis-ui-{short}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name])
            .env("RUSTGIS_TEST_ISOLATED", "1")
            .env(rustgis_engine::data::dirs::CONFIG_ENV, &dir)
            .env("RUSTGIS_LANGUAGE", "en")
            .output()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains("1 passed"), "the isolated run didn't run {name}:\n{stdout}");
        false
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn saved_accounts_load_models_at_startup_and_on_provider_and_account_switch() {
        use egui_kittest::kittest::Queryable;
        if !in_isolated_settings("assistantpane::tests::saved_accounts_load_models_at_startup_and_on_provider_and_account_switch") {
            return;
        }
        let a = chatgpt::Credentials {
            client_id: "test-personal".into(),
            subject: "test-user".into(),
            email: "same@example.test".into(),
            // No expiry and no refresh token: a request fails at renewal, without network I/O.
            access_token: "synthetic-token".into(),
            scopes: vec![chatgpt::PLAN_SCOPE.into()],
            ..Default::default()
        };
        let b = chatgpt::Credentials { client_id: "test-workspace".into(), ..a.clone() };
        let path = chatgpt::auth_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // As versions before the keychain saved them, with their tokens; this run has no keychain,
        // so they stay there.
        let saved = |c: &chatgpt::Credentials| {
            let mut v = c.to_json();
            v["access_token"] = c.access_token.clone().into();
            v
        };
        std::fs::write(
            &path,
            serde_json::json!({"version":2, "active":{"client_id":a.client_id,"subject":a.subject},
            "accounts":[saved(&a),saved(&b)]})
            .to_string(),
        )
        .unwrap();
        let mut app = RustGisApp::with_sample();
        app.services.load_pref = Some(Box::new(|key| (key == "assistant_provider").then(|| "chatgpt".into())));
        ensure_loaded(&mut app, &egui::Context::default());
        // The sign-in is read on a worker; the catalog request follows once it arrives.
        let ctx = egui::Context::default();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.assistant.models_slot.is_none() && app.assistant.models_error.is_none() {
            assert!(std::time::Instant::now() < end, "the sign-in read never finished");
            std::thread::sleep(std::time::Duration::from_millis(1)); // test-wait: legacy polling for saved-account models
            poll(&mut app, &ctx);
        }
        assert_eq!(app.assistant.account.as_ref().map(|a| a.kept), Some(chatgpt::Kept::SettingsFile));
        app.run("ui.pane.assistant", serde_json::Value::Null).unwrap();
        app.assistant.settings = true;
        // Draw settings without polling, so each click's catalog request remains observable.
        // A fixed number of frames, not `run`: each catalog request ends on a thread that asks for a
        // repaint, which can land mid-run (it did on Windows) and run past `run`'s step limit.
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(400.0, 600.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    ui.ctx().request_repaint();
                    return;
                }
                settings(app, ui, &Tokens::light());
            },
            app,
        );
        h.run_steps(4);
        h.state_mut().assistant.models_slot = None;
        h.get_by_value("ChatGPT (sign in)").click();
        h.run_steps(4);
        h.get_by_label("Ollama (local)").click();
        h.run_steps(4);
        assert!(h.state().assistant.models_slot.is_none());
        h.get_by_value("Ollama (local)").click();
        h.run_steps(4);
        h.get_by_label("ChatGPT (sign in)").click();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while h.state().assistant.models_slot.is_none() {
            assert!(std::time::Instant::now() < end, "the sign-in read never finished");
            std::thread::sleep(std::time::Duration::from_millis(1)); // test-wait: legacy polling for switched-account models
            h.run_steps(1);
        }
        h.state_mut().assistant.models_slot = None;
        // A read begun before choosing an account must not undo the choice when it finishes.
        let stale: Slot<SignInResult> = Slot::default();
        h.state_mut().assistant.sign_in_read = Some((None, stale.clone()));
        h.get_by_value(&a.label()).click();
        h.run_steps(4);
        h.get_by_label(&b.label()).click();
        h.run_steps(4);
        *stale.lock().unwrap() = Some((Some(a.clone()), None));
        h.run_steps(4);
        assert!(h.state().assistant.models_slot.is_some());
        assert_eq!(h.state().assistant.agent.as_ref().unwrap().config.client_id, b.client_id);
        assert_eq!(chatgpt::load_any().unwrap().client_id, b.client_id);
    }

    /// A refused store is distinct from no saved account and reaches notices once, not just logs.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_refused_sign_in_read_is_visible_once_and_a_clean_read_is_quiet() {
        let ctx = egui::Context::default();
        let mut app = plan_app();
        app.session.notices.clear();
        let finish = |app: &mut RustGisApp, warning: Option<rustgis_assistant::Error>| {
            let slot: Slot<SignInResult> = Slot::default();
            *slot.lock().unwrap() = Some((None, warning));
            app.assistant.sign_in_read = Some((None, slot));
            poll_sign_in(app, &ctx);
        };
        finish(&mut app, None);
        assert!(app.session.notices.is_empty());
        let warning =
            rustgis_assistant::Error::new(rustgis_assistant::Recovery::Auth, "the ChatGPT sign-ins were saved by a newer RustGIS");
        finish(&mut app, Some(warning.clone()));
        finish(&mut app, Some(warning));
        assert_eq!(app.session.notices.len(), 1);
        assert!(app.session.notices[0].text.contains("newer RustGIS"));
        assert!(app.assistant.login_error.as_ref().unwrap().detail.contains("newer RustGIS"));
        let mut other = plan_app();
        other.assistant.agent.as_mut().unwrap().config = Config::from_env("custom");
        other.session.notices.clear();
        finish(&mut other, Some("a stale read for another provider".into()));
        assert!(other.session.notices.is_empty() && other.assistant.login_error.is_none());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_sign_in_check_never_switches_accounts_and_a_switch_stops_work() {
        if !in_isolated_settings("assistantpane::tests::a_sign_in_check_never_switches_accounts_and_a_switch_stops_work") {
            return;
        }
        let ctx = egui::Context::default();
        let creds = |client: &str| chatgpt::Credentials {
            client_id: client.into(),
            subject: "user".into(),
            session: "session".into(),
            access_token: "synthetic-token".into(),
            scopes: vec![chatgpt::PLAN_SCOPE.into()],
            ..Default::default()
        };
        let finish = |app: &mut RustGisApp, checking: Option<chatgpt::Binding>, read: Option<chatgpt::Credentials>| {
            let slot: Slot<SignInResult> = Slot::default();
            *slot.lock().unwrap() = Some((read, None));
            app.assistant.sign_in_read = Some((checking, slot));
            poll_sign_in(app, &ctx);
        };
        let mut app = plan_app();
        let mut first = app.assistant.agent.as_ref().unwrap().config.clone();
        first.sign_in(&creds("first"));
        apply_config(&mut app, first.clone());
        // A check after a failed request that finds another account selected changes nothing:
        // the user chooses.
        app.assistant.retry = Some((1, web_time::Instant::now()));
        finish(&mut app, Some(first.binding()), Some(creds("second")));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config, first);
        assert!(app.assistant.retry.is_some());
        // One that finds the sign-in gone clears it and stops the work bound to it, and a model
        // list still on its way can't bring its catalog back.
        let late: Reply = Slot::default();
        app.assistant.models_slot = Some(late.clone());
        app.assistant.models = vec!["first-model".into()];
        // A message typed while the check was under way, and a catalog request it deferred.
        app.assistant.queued = Some("synthetic question".into());
        app.assistant.catalog_pending = true;
        finish(&mut app, Some(first.binding()), None);
        assert!(app.assistant.agent.as_ref().unwrap().config.key.is_empty());
        assert!(app.assistant.retry.is_none() && app.assistant.settings);
        assert!(app.assistant.models.is_empty() && app.assistant.models_slot.is_none());
        *late.lock().unwrap() = Some(Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: serde_json::json!({"models": [{"slug": "late", "visibility": "list"}]}).to_string().into_bytes(),
        }));
        poll(&mut app, &ctx);
        assert!(app.assistant.models.is_empty() && app.assistant.catalog.is_empty());
        // Neither goes out after the sign-out; the message is back in the input box.
        assert!(app.assistant.queued.is_none() && !app.assistant.catalog_pending);
        assert!(app.assistant.inflight.is_none() && app.assistant.models_slot.is_none());
        assert_eq!(app.assistant.input, "synthetic question");
        // Taking the selected account: another one stops work for the first and drops its catalog.
        apply_config(&mut app, first.clone());
        app.assistant.retry = Some((2, web_time::Instant::now()));
        app.assistant.models = vec!["first-model".into()];
        finish(&mut app, None, Some(creds("second")));
        assert_eq!(app.assistant.agent.as_ref().unwrap().config.client_id, "second");
        assert!(app.assistant.retry.is_none());
        assert!(!app.assistant.models.contains(&"first-model".to_string()));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn sign_in_loads_catalog_and_applies_first_offered_model() {
        if !in_isolated_settings("assistantpane::tests::sign_in_loads_catalog_and_applies_first_offered_model") {
            return;
        }
        let mut app = plan_app();
        let c = chatgpt::Credentials {
            access_token: "synthetic-token".into(),
            client_id: "client".into(),
            subject: "user".into(),
            scopes: vec![chatgpt::PLAN_SCOPE.into()],
            ..Default::default()
        };
        let ctx = egui::Context::default();
        signed_in(&mut app, &c, &ctx);
        assert!(app.assistant.models_slot.is_some());
        let slot = Slot::default();
        *slot.lock().unwrap() = Some(Ok(HttpResponse { status: 200, headers: vec![],
            body: serde_json::json!({"models":[{"slug":"offered", "display_name":"Offered model", "visibility":"list", "supports_reasoning":false}]}).to_string().into_bytes() }));
        app.assistant.models_slot = Some(slot);
        poll(&mut app, &ctx);
        let cfg = &app.assistant.agent.as_ref().unwrap().config;
        assert_eq!(cfg.model, "offered");
        assert_eq!(cfg.reasoning, Some(false));
        assert_eq!(app.assistant.catalog[0].name, "Offered model");
        signed_in(&mut app, &chatgpt::Credentials { scopes: vec![], ..c }, &ctx);
        assert!(app.assistant.models_slot.is_none());
        assert!(app.assistant.agent.as_ref().unwrap().config.key.is_empty());
        assert!(!app.assistant.welcome);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_plan_requests_cannot_bypass_auth_through_host_http() {
        if !in_isolated_settings("assistantpane::tests::native_plan_requests_cannot_bypass_auth_through_host_http") {
            return;
        }
        let mut app = plan_app();
        app.services.http = Some(Box::new(|_, _| panic!("the host hook must not receive plan credentials")));
        let request = HttpRequest { method: "GET", url: format!("{}/models", chatgpt::API), headers: vec![], body: None };
        let slot = dispatch(&mut app, request, Some(chatgpt::Binding::default()), None, &egui::Context::default());
        let start = std::time::Instant::now();
        loop {
            if let Some(result) = take(&slot) {
                assert_eq!(result.unwrap_err().recovery, rustgis_assistant::Recovery::SignIn);
                break;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::yield_now();
        }
    }

    #[test]
    fn usage_error_offers_weekly_usage() {
        use egui_kittest::kittest::Queryable;
        let error = rustgis_assistant::Error::new(rustgis_assistant::Recovery::Usage, "");
        let mut app = plan_app();
        app.assistant.settings = false;
        app.assistant.agent.as_mut().unwrap().transcript.push(Entry::Error(error));
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Weekly usage").is_some());
        h.get_by_label("Weekly usage").click();
        h.step();
        assert!(
            h.output().platform_output.commands.iter().any(|c| {
                matches!(c, egui::OutputCommand::OpenUrl(url) if url.url == "https://chatgpt.com/settings/usage?tab=overview")
            })
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn late_sign_in_never_changes_another_providers_key() {
        if !in_isolated_settings("assistantpane::tests::late_sign_in_never_changes_another_providers_key") {
            return;
        }
        let mut app = RustGisApp::with_sample();
        let cfg = Config {
            provider: "custom".into(),
            base_url: "http://localhost:1234/v1".into(),
            model: "test".into(),
            key: "original-key".into(),
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
        };
        app.assistant.agent = Some(Agent::new(cfg.clone(), &app.session));
        let c = chatgpt::Credentials { access_token: "synthetic-chatgpt-token".into(), ..Default::default() };
        signed_in(&mut app, &c, &egui::Context::default());
        assert_eq!(app.assistant.agent.as_ref().unwrap().config, cfg);
        assert!(app.assistant.models_slot.is_none());
    }

    #[test]
    fn provider_change_stops_the_previous_request() {
        let mut app = RustGisApp::with_sample();
        let cfg = Config {
            provider: "custom".into(),
            base_url: "http://localhost:1234/v1".into(),
            model: "test".into(),
            key: String::new(),
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
        };
        let mut agent = Agent::new(cfg.clone(), &app.session);
        assert!(matches!(agent.ask("hello", &app.session), Step::Send(_)));
        let generation = agent.generation();
        app.assistant.agent = Some(agent);
        app.assistant.inflight = Some((generation, Slot::default()));
        apply_config(&mut app, Config { provider: "openai".into(), ..cfg });
        let agent = app.assistant.agent.as_ref().unwrap();
        assert!(!agent.busy());
        assert_ne!(agent.generation(), generation);
        assert!(app.assistant.inflight.is_none());
    }

    /// The right pane's width with `entries` in the conversation and `model` as the model's name.
    fn pane_width(entries: Vec<Entry>, model: &str) -> f32 {
        let h = chat(entries, model);
        right_pane(&h).width()
    }

    fn right_pane(h: &egui_kittest::Harness<'_, RustGisApp>) -> egui::Rect {
        h.state().ui.pane_rects[1].unwrap()
    }

    /// The window with the assistant open and `entries` in the conversation.
    fn chat(entries: Vec<Entry>, model: &str) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        h.state_mut().run("ui.pane.assistant", serde_json::Value::Null).unwrap();
        for _ in 0..4 {
            h.step();
        }
        let app = h.state_mut();
        app.assistant.settings = false;
        let agent = app.assistant.agent.as_mut().unwrap();
        agent.config.model = model.into();
        agent.transcript = entries;
        for _ in 0..6 {
            h.step();
        }
        h
    }

    /// #307: a reply wider than the pane pushed its edge off the window, so the map covered the
    /// chat and the edge could not be dragged. The pane stays in the window and keeps resizing.
    #[test]
    fn wide_reply_keeps_the_pane_in_the_window_and_resizable() {
        let sentence = "The assistant ran the buffer tool on the cities layer and added the result to the map. ".repeat(4);
        let mut h = chat(vec![Entry::Assistant(format!("{}- {sentence}", " ".repeat(400)))], "m");
        for _ in 0..20 {
            h.step();
        }
        let before = right_pane(&h);
        let window = h.ctx.content_rect();
        assert!(before.max.x <= window.max.x + 0.5, "the pane's edge left the window: {before:?} in {window:?}");
        // Drag the pane's edge 150 points towards the map (grabbed just inside the pane, as the
        // map's own drag sense is on top at the boundary).
        let edge = egui::pos2(before.min.x + 2.0, before.center().y);
        h.hover_at(edge);
        h.step();
        h.drag_at(edge);
        h.step();
        for dx in [50.0, 100.0, 150.0] {
            h.hover_at(edge - vec2(dx, 0.0));
            h.step();
        }
        h.event(egui::Event::PointerButton {
            pos: edge - vec2(150.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..4 {
            h.step();
        }
        let after = right_pane(&h);
        assert!((after.min.x - (edge.x - 150.0)).abs() < 3.0, "the pane did not follow the drag: {before:?} to {after:?}");
        assert!(after.max.x <= window.max.x + 0.5, "the pane's edge left the window: {after:?} in {window:?}");
    }

    /// Dragging the grip above the input box up makes the box taller; Send stays in the window.
    #[test]
    fn input_box_resizes_by_its_grip() {
        use egui_kittest::kittest::Queryable;
        let mut h = chat(Vec::new(), "m");
        let input = |h: &egui_kittest::Harness<'_, RustGisApp>| h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect();
        let before = input(&h);
        let grip = egui::pos2(before.center().x, before.min.y - 4.0);
        h.hover_at(grip);
        h.step();
        h.drag_at(grip);
        h.step();
        for dy in [40.0, 80.0, 120.0] {
            h.hover_at(grip - vec2(0.0, dy));
            h.step();
        }
        h.event(egui::Event::PointerButton {
            pos: grip - vec2(0.0, 120.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..4 {
            h.step();
        }
        let after = input(&h);
        assert!((after.height() - before.height() - 120.0).abs() < 4.0, "the box did not follow the drag: {before:?} to {after:?}");
        assert!(h.get_by_label("Send").rect().max.y <= h.ctx.content_rect().max.y);
    }

    #[test]
    fn long_lines_wrap_instead_of_widening_the_pane() {
        let sentence = "The assistant ran the buffer tool on the cities layer and added the result to the map. ".repeat(4);
        let empty = pane_width(Vec::new(), "m");
        let mut widened = Vec::new();
        for (what, entry, model) in [
            ("a list item", Entry::Assistant(format!("- {sentence}")), "m"),
            ("a numbered item", Entry::Assistant(format!("1. {sentence}")), "m"),
            ("a quote", Entry::Assistant(format!("> {sentence}")), "m"),
            ("a word with no spaces", Entry::Assistant(format!("- {}", "x".repeat(400))), "m"),
            ("a deeply indented item", Entry::Assistant(format!("{}- {sentence}", " ".repeat(200))), "m"),
            ("the user's message", Entry::User(sentence.clone()), "m"),
            (
                "a tool call",
                Entry::Tool { name: "run_command".into(), summary: "gp.run ".repeat(20), result: sentence.clone(), error: false },
                "m",
            ),
            (
                "the model's name",
                Entry::Assistant("Done.".into()),
                "provider/a-very-long-model-name-with-many-parts-preview-2026-10-14-extended",
            ),
        ] {
            let w = pane_width(vec![entry], model);
            if (w - empty).abs() > 0.5 {
                widened.push(format!("{what}: {empty} to {w}"));
            }
        }
        assert!(widened.is_empty(), "the pane grew wider with {widened:?}");
    }
}
