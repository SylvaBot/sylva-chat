//! The preferences window: general settings, ignores and highlights.

use super::*;
use crate::settings::{Pattern, Settings};

#[derive(Clone, Copy, PartialEq)]
pub enum PrefsTab {
    General,
    Ignores,
    Highlights,
}

impl PrefsTab {
    fn title(self) -> &'static str {
        match self {
            PrefsTab::General => "General",
            PrefsTab::Ignores => "Ignores",
            PrefsTab::Highlights => "Highlights",
        }
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(14.0);
    ui.label(egui::RichText::new(text).font(semibold(12.5)).color(MUTED));
    ui.add_space(2.0);
}

/// A label on the left and an on/off switch on the right.
fn switch_row(ui: &mut egui::Ui, label: &str, on: &mut bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, R_CONTROL, BTN);
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    ui.painter().text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, regular(13.5), TEXT);
    let sw = Rect::from_center_size(pos2(rect.right() - 30.0, rect.center().y), vec2(36.0, 20.0));
    ui.painter().rect_filled(sw, CornerRadius::same(10), if *on { SAGE } else { SURFACE_3 });
    let knob_x = ui.ctx().animate_value_with_time(resp.id.with("k"), if *on { 1.0 } else { 0.0 }, 0.12);
    let cx = sw.left() + 10.0 + 16.0 * knob_x;
    ui.painter().circle_filled(pos2(cx, sw.center().y), 7.0, if *on { Color32::from_rgb(0x1a, 0x24, 0x14) } else { TEXT_2 });
    if resp.clicked() {
        *on = !*on;
        return true;
    }
    false
}

/// A label with a row of choices on the right; one is picked.
fn choice_row(ui: &mut egui::Ui, label: &str, value: &mut u8, options: &[&str]) {
    ui.horizontal(|ui| {
        ui.set_min_height(36.0);
        ui.add_space(10.0);
        ui.label(egui::RichText::new(label).font(regular(13.5)).color(TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (i, o) in options.iter().enumerate().rev() {
                let on = *value as usize == i;
                let g = ui.painter().layout_no_wrap((*o).into(), medium(12.5), if on { Color32::from_rgb(0x1a, 0x24, 0x14) } else { TEXT });
                let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 30.0), Sense::click());
                ui.painter().rect_filled(rect, R_CONTROL, if on { SAGE } else if resp.hovered() { BTN_IN_HOVER } else { BTN_IN });
                ui.painter().galley(rect.center() - g.size() / 2.0, g, TEXT);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    *value = i as u8;
                }
            }
        });
    });
}

/// A label with a number the person types.
fn number_row(ui: &mut egui::Ui, label: &str, value: &mut u32, min: u32, max: u32, id: &str) {
    ui.horizontal(|ui| {
        ui.set_min_height(36.0);
        ui.add_space(10.0);
        ui.label(egui::RichText::new(label).font(regular(13.5)).color(TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let mut text = value.to_string();
            let (rect, _) = ui.allocate_exact_size(vec2(88.0, 30.0), Sense::hover());
            ui.painter().rect_filled(rect, R_CONTROL, BTN_IN);
            let r = ui.put(rect.shrink2(vec2(8.0, 0.0)), egui::TextEdit::singleline(&mut text).id(Id::unique(("num", id))).frame(egui::Frame::NONE).font(mono(13.0)).text_color(TEXT).align(egui::Align2::RIGHT_CENTER));
            if r.changed() {
                if let Ok(n) = text.trim().parse::<u32>() {
                    *value = n.clamp(min, max);
                } else if text.trim().is_empty() {
                    *value = min;
                }
            }
        });
    });
}

impl App {
    pub(super) fn open_prefs(&mut self) {
        self.prefs_draft = crate::settings::get().as_ref().clone();
        self.prefs_open = Some(PrefsTab::General);
        self.menu = false;
    }

    pub(super) fn prefs_window(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.prefs_open else { return };
        let screen = ctx.content_rect();
        let w = 560.0_f32.min(screen.width() - 24.0);
        let h = (screen.height() - 64.0).clamp(240.0, 640.0);
        let pos = pos2(screen.center().x - w / 2.0, (screen.center().y - h / 2.0).max(TITLEBAR + 8.0));
        // Dim everything behind, and swallow clicks on it.
        let layer = egui::LayerId::new(Order::Middle, Id::unique("prefs-scrim"));
        ctx.layer_painter(layer).rect_filled(screen, 0.0, Color32::from_black_alpha(140));
        egui::Area::new(Id::unique("prefs-scrim-hit")).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
            ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
        });
        let mut draft = self.prefs_draft.clone();
        let mut close = false;
        let mut next = tab;
        egui::Area::new(Id::unique("prefs")).order(Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            popup_frame(R_PANEL, 20).show(ui, |ui| {
                ui.set_width(w - 40.0);
                ui.set_height(h - 40.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Preferences").font(bold(17.0)).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                        if self.win_button(ui, r, "x", false, false, "Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    for t in [PrefsTab::General, PrefsTab::Ignores, PrefsTab::Highlights] {
                        let on = t == tab;
                        let g = ui.painter().layout_no_wrap(t.title().into(), medium(13.5), if on { TEXT } else { MUTED });
                        let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 24.0, 32.0), Sense::click());
                        ui.painter().rect_filled(rect, R_CONTROL, if on { BTN } else if resp.hovered() { BTN_IN } else { Color32::TRANSPARENT });
                        ui.painter().galley(rect.center() - g.size() / 2.0, g, TEXT);
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            next = t;
                        }
                    }
                });
                ui.add_space(6.0);
                egui::ScrollArea::vertical().id_salt(("prefs-scroll", tab.title())).auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    match tab {
                        PrefsTab::General => general(ui, &mut draft),
                        PrefsTab::Ignores => ignores(ui, &mut draft),
                        PrefsTab::Highlights => highlights(ui, &mut draft),
                    }
                });
            });
        });
        self.prefs_open = Some(next);
        if draft != self.prefs_draft {
            self.prefs_draft = draft.clone();
            crate::settings::set(draft);
            // Every row is laid out again with the new settings.
            for r in self.rows.iter_mut().chain(self.held.iter_mut()) {
                r.layout = None;
            }
            ctx.request_repaint();
        }
        if close || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.prefs_open = None;
        }
    }
}

fn general(ui: &mut egui::Ui, s: &mut Settings) {
    heading(ui, "Messages");
    switch_row(ui, "Show timestamps", &mut s.timestamps);
    choice_row(ui, "Timestamp format", &mut s.time_format, &["h:mm", "hh:mm", "h:mm AM", "h:mm:ss"]);
    switch_row(ui, "Separate messages with lines", &mut s.separators);
    switch_row(ui, "Alternate background color", &mut s.alternate_rows);
    switch_row(ui, "Dim message history", &mut s.dim_history);
    switch_row(ui, "Show the message a reply answers", &mut s.reply_context);
    switch_row(ui, "Hide deleted messages", &mut s.hide_deleted);
    number_row(ui, "Deleted messages shown up to (characters, 0 for all)", &mut s.deleted_len, 0, 500, "deleted");
    switch_row(ui, "Show the shared chat label", &mut s.shared_badge);

    heading(ui, "Mentions and completion");
    switch_row(ui, "Bold @usernames", &mut s.bold_mentions);
    switch_row(ui, "Color @usernames", &mut s.color_mentions);
    switch_row(ui, "Complete usernames with Tab", &mut s.name_complete);
    switch_row(ui, "Add a comma after a name at the start", &mut s.mention_comma);
    switch_row(ui, "Complete emotes with Tab", &mut s.emote_complete);

    heading(ui, "Emotes and emoji");
    switch_row(ui, "Show emotes", &mut s.emotes);
    switch_row(ui, "Animate emotes", &mut s.animate);
    switch_row(ui, "Animate only when the window is focused", &mut s.animate_focused_only);
    switch_row(ui, "Space between emotes", &mut s.emote_gap);
    choice_row(ui, "Emote size", &mut s.emote_size, &["Small", "Default", "Large"]);
    switch_row(ui, "Enlarge emotes, emoji and badges on hover", &mut s.hover_cards);
    switch_row(ui, "Show emoji as color pictures", &mut s.twemoji);

    heading(ui, "Visible badges");
    switch_row(ui, "Authority (broadcaster, moderator, VIP, staff)", &mut s.badge_authority);
    switch_row(ui, "Predictions", &mut s.badge_prediction);
    switch_row(ui, "Subscriber", &mut s.badge_subscriber);
    switch_row(ui, "Vanity (bits, Turbo, Prime and others)", &mut s.badge_vanity);

    heading(ui, "Chat");
    switch_row(ui, "Pause while the pointer moves over chat", &mut s.pause_on_hover);
    switch_row(ui, "Show message length while typing", &mut s.show_length);
    switch_row(ui, "Load message history on connect", &mut s.load_history);
    number_row(ui, "Messages to load on connect (up to 800)", &mut s.history_limit, 1, 800, "history");
    number_row(ui, "Messages kept per person for their card", &mut s.usercard_limit, 50, 5000, "usercard");
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Message history and card limits apply the next time chat opens.").font(regular(12.0)).color(FAINT));
}

fn ignores(ui: &mut egui::Ui, s: &mut Settings) {
    heading(ui, "Blocked users");
    ui.label(egui::RichText::new("/block name and /unblock name in the chat box work too. You can also block someone from their card.").font(regular(12.0)).color(FAINT));
    ui.add_space(6.0);
    switch_row(ui, "Show blocked users' messages (faded)", &mut s.show_blocked);
    let id = Id::unique("block-add");
    let mut add = String::new();
    let mut go = false;
    ui.horizontal(|ui| {
        let mut text = ui.memory_mut(|m| m.data.get_temp::<String>(id).unwrap_or_default());
        let enter = ui.memory(|m| m.has_focus(id.with("f"))) && ui.input(|i| i.key_pressed(Key::Enter));
        ui.allocate_ui(vec2(ui.available_width() - 76.0, 36.0), |ui| {
            field(ui, &mut text, "Add a user to block", false, id.with("f"));
        });
        go = small_button(ui, "Block", false).clicked() || enter;
        if go {
            add = std::mem::take(&mut text);
        }
        ui.memory_mut(|m| m.data.insert_temp(id, text));
    });
    if go {
        s.block(&add);
    }
    let mut remove: Option<String> = None;
    if s.blocked.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("No one is blocked.").font(regular(13.0)).color(FAINT));
    }
    for b in &s.blocked {
        ui.horizontal(|ui| {
            ui.set_min_height(32.0);
            ui.add_space(10.0);
            ui.label(egui::RichText::new(b).font(mono(13.0)).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if small_button(ui, "Unblock", false).clicked() {
                    remove = Some(b.clone());
                }
            });
        });
    }
    if let Some(b) = remove {
        s.unblock(&b);
    }

    heading(ui, "Ignored messages");
    ui.label(egui::RichText::new("Hide messages that match a pattern, or swap the matching words. Patterns run top to bottom.").font(regular(12.0)).color(FAINT));
    ui.add_space(6.0);
    if small_button(ui, "Add a pattern", false).clicked() {
        s.patterns.push(Pattern::default());
    }
    ui.add_space(6.0);
    let n = s.patterns.len();
    let mut del: Option<usize> = None;
    let mut swap: Option<(usize, usize)> = None;
    for i in 0..n {
        let invalid = s.patterns[i].regex && s.patterns[i].compile().is_none() && !s.patterns[i].pattern.is_empty();
        let p = &mut s.patterns[i];
        egui::Frame::new().fill(SURFACE).corner_radius(R_ITEM).inner_margin(12).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            field(ui, &mut p.pattern, "Pattern", p.regex, Id::unique(("pat", i)));
            if invalid {
                ui.label(egui::RichText::new("This is not a valid regular expression.").font(regular(12.0)).color(DANGER));
            }
            switch_row(ui, "Regular expression", &mut p.regex);
            switch_row(ui, "Case sensitive", &mut p.case_sensitive);
            switch_row(ui, "Hide the whole message", &mut p.block);
            if !p.block {
                field(ui, &mut p.replacement, "Replacement", false, Id::unique(("rep", i)));
            }
            ui.horizontal(|ui| {
                if i > 0 && small_button(ui, "Move up", false).clicked() {
                    swap = Some((i, i - 1));
                }
                if i + 1 < n && small_button(ui, "Move down", false).clicked() {
                    swap = Some((i, i + 1));
                }
                if small_button(ui, "Remove", true).clicked() {
                    del = Some(i);
                }
            });
        });
        ui.add_space(6.0);
    }
    if let Some((a, b)) = swap {
        s.patterns.swap(a, b);
    }
    if let Some(i) = del {
        s.patterns.remove(i);
    }
}

fn highlights(ui: &mut egui::Ui, s: &mut Settings) {
    heading(ui, "Stand out in chat");
    ui.label(egui::RichText::new("Each has its own color and a label on the message.").font(regular(12.0)).color(FAINT));
    ui.add_space(6.0);
    for (mark, label, on) in [
        (Mark::FirstStream, "A person's first message this stream", &mut s.hl_first_stream),
        (Mark::FirstEver, "A person's first message ever", &mut s.hl_first_ever),
        (Mark::Suspicious, "People Twitch flags as suspicious or restricted", &mut s.hl_suspicious),
    ] {
        let (rect, _) = ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover());
        let _ = rect;
        ui.horizontal(|ui| {
            let (sw, _) = ui.allocate_exact_size(vec2(10.0, 36.0), Sense::hover());
            ui.painter().rect_filled(Rect::from_center_size(sw.center(), vec2(8.0, 20.0)), CornerRadius::same(4), mark.color());
            ui.vertical(|ui| {
                switch_row(ui, label, on);
            });
        });
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Restricted people show in red, monitored ones in orange. Turn highlights off for one person from their card.").font(regular(12.0)).color(FAINT));
}
