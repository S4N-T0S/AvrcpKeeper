use eframe::egui::{
    self, emath::GuiRounding, style::WidgetVisuals, Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Pos2,
    Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType,
};

use super::theme;

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

fn focus_ring(ui: &Ui, response: &Response, rect: Rect, radius: u8) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(radius.saturating_add(2)),
            Stroke::new(2.0, theme::ACCENT),
            StrokeKind::Outside,
        );
    }
}

fn paint_switch(ui: &Ui, rect: Rect, on: bool, hovered: bool, id: Id) {
    let t = ui.ctx().animate_bool_responsive(id, on);
    let painter = ui.painter();
    let radius = CornerRadius::same(11);
    let on_fill = if hovered { theme::ACCENT_HOVER } else { theme::ACCENT };
    painter.rect_filled(rect, radius, mix(Color32::TRANSPARENT, on_fill, t));
    if t < 1.0 {
        let edge = if hovered { theme::TEXT_DIM } else { theme::TEXT_FAINT };
        painter.rect_stroke(rect, radius, Stroke::new(1.0, edge.gamma_multiply(1.0 - t)), StrokeKind::Inside);
    }
    let x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
    let knob_r = egui::lerp(6.0..=8.0, t);
    painter.circle_filled(Pos2::new(x, rect.center().y), knob_r, mix(theme::TEXT_DIM, Color32::WHITE, t));
}

/// The box and tick of a checkbox, `t` going from 0 (off) to 1 (on).
fn paint_check(ui: &Ui, rect: Rect, t: f32, hovered: bool) {
    let painter = ui.painter();
    let radius = CornerRadius::same(5);
    if hovered {
        painter.rect_filled(rect, radius, theme::SURFACE_HI);
    }
    let (edge, on_fill) =
        if hovered { (theme::TEXT_DIM, theme::ACCENT_HOVER) } else { (theme::TEXT_FAINT, theme::ACCENT) };
    painter.rect(
        rect,
        radius,
        mix(Color32::TRANSPARENT, on_fill, t),
        Stroke::new(1.0, mix(edge, on_fill, t)),
        StrokeKind::Inside,
    );
    if t > 0.0 {
        let c = rect.center();
        let check = vec![c + egui::vec2(-4.5, 0.2), c + egui::vec2(-1.3, 3.4), c + egui::vec2(4.8, -3.6)];
        painter.line(check, Stroke::new(2.0, Color32::WHITE.gamma_multiply(t)));
    }
}

/// A full-width row with a title, an optional subtitle and a switch; clicking anywhere on it
/// toggles. Returns true when the value changed.
pub fn toggle_row(ui: &mut Ui, on: &mut bool, title: &str, subtitle: Option<&str>) -> bool {
    let width = ui.available_width();
    let text_w = (width - 54.0).max(40.0);
    let title_galley =
        ui.painter().layout(title.to_owned(), FontId::proportional(theme::SIZE_BODY), theme::TEXT, text_w);
    let sub_galley = subtitle.map(|s| {
        ui.painter().layout(s.to_owned(), FontId::proportional(theme::SIZE_SECONDARY), theme::TEXT_DIM, text_w)
    });
    let text_h = title_galley.size().y + sub_galley.as_ref().map_or(0.0, |g| g.size().y + 2.0);
    let height = (text_h + 12.0).max(32.0);

    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *on, title));
    let changed = response.clicked();
    if changed {
        *on = !*on;
    }
    let painter = ui.painter();
    let hover_rect = rect.expand2(egui::vec2(8.0, 0.0));
    if response.hovered() {
        painter.rect_filled(hover_rect, CornerRadius::same(6), theme::SURFACE_HI);
    }
    let switch = Rect::from_min_size(Pos2::new(rect.right() - 38.0, rect.center().y - 11.0), egui::vec2(38.0, 22.0));
    paint_switch(ui, switch, *on, response.hovered(), response.id);

    let mut y = rect.center().y - text_h / 2.0;
    let title_h = title_galley.size().y;
    painter.galley(Pos2::new(rect.left(), y), title_galley, theme::TEXT);
    if let Some(g) = sub_galley {
        y += title_h + 2.0;
        painter.galley(Pos2::new(rect.left(), y), g, theme::TEXT_DIM);
    }
    focus_ring(ui, &response, hover_rect, 6);
    response.on_hover_cursor(CursorIcon::PointingHand);
    changed
}

/// A small "?" that shows `text` on hover, and stays open after a click or while focused.
pub fn help(ui: &mut Ui, topic: &str, text: &str) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("About {topic}")));
    let pinned_id = response.id.with("pinned");
    let mut pinned = ui.data(|d| d.get_temp::<bool>(pinned_id).unwrap_or(false));
    if response.clicked() {
        pinned = !pinned;
    } else if pinned && ui.input(|i| i.pointer.any_click() || i.key_pressed(Key::Escape)) {
        pinned = false;
    }
    ui.data_mut(|d| d.insert_temp(pinned_id, pinned));

    let active = response.hovered() || pinned || response.has_focus();
    let painter = ui.painter();
    let c = rect.center();
    if active {
        painter.circle_filled(c, 8.0, theme::SURFACE_HOVER);
    }
    painter.circle_stroke(c, 7.5, Stroke::new(1.0, if active { theme::TEXT_DIM } else { theme::TEXT_FAINT }));
    painter.text(
        c + egui::vec2(0.0, 0.5),
        Align2::CENTER_CENTER,
        "?",
        FontId::new(11.0, theme::semibold()),
        if active { theme::TEXT } else { theme::TEXT_DIM },
    );
    focus_ring(ui, &response, rect, 8);
    let contents = |ui: &mut Ui| {
        ui.set_max_width(280.0);
        ui.label(egui::RichText::new(text).color(theme::TEXT).size(theme::SIZE_SECONDARY));
    };
    if pinned || response.has_focus() {
        response.show_tooltip_ui(contents);
    } else {
        response.on_hover_cursor(CursorIcon::PointingHand).on_hover_ui(contents);
    }
}

/// Equal-width segmented control; Left/Right move the selection while it has focus.
/// Returns true when the selection changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let height = 32.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(8), theme::SURFACE_HI);
    let n = options.len().max(1);
    let seg_w = (rect.width() - 6.0) / n as f32;
    let current = options.iter().position(|(o, _)| *o == *value);
    let mut changed = false;
    let mut focus_to = None;
    for (i, (option, label)) in options.iter().enumerate() {
        let seg =
            Rect::from_min_size(rect.min + egui::vec2(3.0 + seg_w * i as f32, 3.0), egui::vec2(seg_w, height - 6.0));
        let response = ui.interact(seg, ui.id().with(("segment", i)), Sense::click());
        let selected = Some(i) == current;
        response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, *label));
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
        if response.has_focus() {
            let step = ui.input(|inp| {
                if inp.key_pressed(Key::ArrowRight) {
                    1
                } else if inp.key_pressed(Key::ArrowLeft) {
                    -1
                } else {
                    0
                }
            });
            if step != 0 {
                let j = (i as isize + step).clamp(0, n as isize - 1) as usize;
                if j != i {
                    *value = options[j].0;
                    changed = true;
                    focus_to = Some(ui.id().with(("segment", j)));
                }
            }
        }

        let t = ui.ctx().animate_bool_responsive(response.id, *value == *option);
        let painter = ui.painter();
        if t > 0.0 {
            painter.rect(
                seg,
                CornerRadius::same(6),
                mix(theme::SURFACE_HI, theme::SELECTED, t),
                Stroke::new(1.0, mix(theme::SURFACE_HI, theme::SELECTED_EDGE, t)),
                StrokeKind::Inside,
            );
        }
        let color = if t > 0.5 || response.hovered() { theme::TEXT } else { theme::TEXT_DIM };
        let family = if t > 0.5 { theme::semibold() } else { egui::FontFamily::Proportional };
        painter.text(seg.center(), Align2::CENTER_CENTER, *label, FontId::new(theme::SIZE_SECONDARY, family), color);
        focus_ring(ui, &response, seg, 6);
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
    if let Some(id) = focus_to {
        ui.memory_mut(|m| m.request_focus(id));
    }
    changed
}

/// Horizontal slider snapping to `step`. Arrow keys adjust it when focused.
pub fn slider(ui: &mut Ui, value: &mut u32, range: std::ops::RangeInclusive<u32>, step: u32, label: &str) -> Response {
    let (lo, hi) = (*range.start(), *range.end());
    let height = 22.0;
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::slider(ui.is_enabled(), f64::from(*value), label));
    let knob_r = 8.0;
    let rail = Rect::from_min_max(
        Pos2::new(rect.left(), rect.center().y - 2.0),
        Pos2::new(rect.right(), rect.center().y + 2.0),
    );
    let travel = (rect.left() + knob_r)..=(rect.right() - knob_r);

    let old = *value;
    if let Some(pos) = response.interact_pointer_pos() {
        let t = ((pos.x - travel.start()) / (travel.end() - travel.start())).clamp(0.0, 1.0);
        let raw = lo as f32 + t * (hi - lo) as f32;
        *value = (((raw / step as f32).round() as u32) * step).clamp(lo, hi);
    }
    if response.has_focus() {
        ui.input(|i| {
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown) {
                *value = value.saturating_sub(step).max(lo);
            }
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp) {
                *value = (*value + step).min(hi);
            }
        });
    }
    if *value != old {
        response.mark_changed();
    }

    let t = (*value - lo) as f32 / (hi - lo).max(1) as f32;
    let knob_x = egui::lerp(travel, t);
    let painter = ui.painter();
    painter.rect_filled(rail, CornerRadius::same(2), theme::SURFACE_HOVER);
    painter.rect_filled(
        Rect::from_min_max(rail.min, Pos2::new(knob_x, rail.max.y)),
        CornerRadius::same(2),
        theme::ACCENT,
    );
    let center = Pos2::new(knob_x, rect.center().y);
    painter.circle_filled(center + egui::vec2(0.0, 1.0), knob_r, Color32::from_black_alpha(70));
    painter.circle_filled(center, knob_r, Color32::WHITE);
    if response.hovered() || response.dragged() {
        painter.circle_stroke(center, knob_r + 3.0, Stroke::new(3.0, theme::ACCENT.gamma_multiply(0.35)));
    }
    if response.has_focus() {
        painter.circle_stroke(center, knob_r + 3.0, Stroke::new(2.0, theme::ACCENT));
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A pill showing `text` with a remove button. Returns true when remove was clicked.
pub fn chip(ui: &mut Ui, text: &str) -> bool {
    let font = FontId::proportional(theme::SIZE_SECONDARY);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, theme::TEXT);
    let size = egui::vec2(galley.size().x + 12.0 + 28.0, 28.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    let x_center = Pos2::new(rect.right() - 15.0, rect.center().y).round_to_pixel_center(painter.pixels_per_point());
    let x_rect = Rect::from_center_size(x_center, egui::vec2(20.0, 20.0));
    let x = ui.interact(x_rect, ui.id().with(("chip", text)), Sense::click());
    x.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("Remove {text}")));
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(14), theme::SURFACE_HI, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
    painter.galley(Pos2::new(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT);
    if x.hovered() || x.has_focus() {
        painter.circle_filled(x_center, 9.0, theme::SURFACE_HOVER);
    }
    if x.has_focus() {
        painter.circle_stroke(x_center, 10.0, Stroke::new(2.0, theme::ACCENT));
    }
    let d = 3.5;
    let stroke = Stroke::new(1.5, if x.hovered() { theme::TEXT } else { theme::TEXT_DIM });
    painter.line_segment([x_center + egui::vec2(-d, -d), x_center + egui::vec2(d, d)], stroke);
    painter.line_segment([x_center + egui::vec2(-d, d), x_center + egui::vec2(d, -d)], stroke);
    x.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A full-width row with a checkbox, a label and a right-aligned status. Returns true on toggle.
pub fn check_row(ui: &mut Ui, checked: &mut bool, label: &str, status: Option<(&str, Color32)>) -> bool {
    let height = 34.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *checked, label));
    let clicked = response.clicked();
    if clicked {
        *checked = !*checked;
    }
    let painter = ui.painter();
    let hover_rect = rect.expand2(egui::vec2(8.0, 0.0));
    if response.hovered() {
        painter.rect_filled(hover_rect, CornerRadius::same(6), theme::SURFACE_HI);
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *checked);
    let bx = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - 9.0), Vec2::splat(18.0));
    paint_check(ui, bx, t, false);

    let mut text_right = rect.right();
    if let Some((status, color)) = status {
        let galley =
            painter.layout_no_wrap(status.to_owned(), FontId::proportional(theme::SIZE_SECONDARY), theme::TEXT_DIM);
        let pos = Pos2::new(rect.right() - galley.size().x, rect.center().y - galley.size().y / 2.0);
        painter.circle_filled(Pos2::new(pos.x - 9.0, rect.center().y), 3.5, color);
        text_right = pos.x - 22.0;
        painter.galley(pos, galley, theme::TEXT_DIM);
    }
    let text_left = bx.right() + 12.0;
    let mut job = egui::text::LayoutJob::single_section(
        label.to_owned(),
        egui::TextFormat::simple(FontId::proportional(theme::SIZE_BODY), theme::TEXT),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((text_right - text_left).max(20.0));
    let galley = painter.layout_job(job);
    painter.galley(Pos2::new(text_left, rect.center().y - galley.size().y / 2.0), galley, theme::TEXT);
    focus_ring(ui, &response, hover_rect, 6);
    response.on_hover_cursor(CursorIcon::PointingHand);
    clicked
}

/// A checkbox sized to its label, unlike `check_row`. Returns true on toggle.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> bool {
    let galley = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(theme::SIZE_BODY), theme::TEXT);
    let size = egui::vec2(18.0 + 12.0 + galley.size().x, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *checked, label));
    let clicked = response.clicked();
    if clicked {
        *checked = !*checked;
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *checked);
    let bx = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - 9.0), Vec2::splat(18.0));
    paint_check(ui, bx, t, response.hovered());
    let pos = Pos2::new(bx.right() + 12.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, theme::TEXT);
    focus_ring(ui, &response, bx, 5);
    response.on_hover_cursor(CursorIcon::PointingHand);
    clicked
}

pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (state, fill) in [
            (&mut w.inactive, theme::BUTTON),
            (&mut w.hovered, theme::BUTTON_HOVER),
            (&mut w.active, theme::BUTTON_PRESSED),
        ] {
            state.weak_bg_fill = fill;
            state.bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
        }
        let button = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).family(theme::semibold()))
            .corner_radius(CornerRadius::same(7))
            .min_size(egui::vec2(84.0, 30.0));
        let response = ui.add(button);
        focus_ring(ui, &response, response.rect, 7);
        response.on_hover_cursor(CursorIcon::PointingHand)
    })
    .inner
}

pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    let button = egui::Button::new(egui::RichText::new(text).color(theme::TEXT))
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(84.0, 30.0));
    ui.add(button).on_hover_cursor(CursorIcon::PointingHand)
}

/// Thin chevron for combo boxes, pointing up while open.
pub fn chevron(ui: &Ui, rect: Rect, _visuals: &WidgetVisuals, open: bool) {
    let c = rect.center();
    let (w, h) = (4.0, 2.0);
    let (tip, wing) = if open { (-h, h) } else { (h, -h) };
    ui.painter().line(
        vec![c + egui::vec2(-w, wing), c + egui::vec2(0.0, tip), c + egui::vec2(w, wing)],
        Stroke::new(1.5, theme::TEXT_DIM),
    );
}
