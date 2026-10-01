//! The settings window's look: its palette for light and dark, the Inter typeface, and the
//! pieces every page is built from (cards, rows, switches, the sidebar, icons, the two screens).

use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontData, FontDefinitions, FontFamily, FontId, Layout as Flow, Pos2, Rect,
    RichText, Sense, Shadow, Shape, Stroke, StrokeKind, TextStyle, Theme, Vec2, Visuals, WidgetInfo, WidgetType,
};

/// Card and window corners; controls are rounder than text, softer than pills.
const CARD_RADIUS: u8 = 12;
const CONTROL_RADIUS: u8 = 8;

/// The colours of one theme. Everything on screen comes from here.
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub border: Color32,
    pub control: Color32,
    pub hover: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub accent: Color32,
    pub warn: Color32,
    pub danger: Color32,
}

impl Palette {
    /// Bezels and stands: between the border and the weak text, so they show on either theme.
    pub fn frame(&self) -> Color32 {
        lerp_color(self.border, self.weak, 0.35)
    }
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0e, 0x10, 0x14),
    sidebar: Color32::from_rgb(0x12, 0x15, 0x1a),
    card: Color32::from_rgb(0x17, 0x1a, 0x21),
    border: Color32::from_rgb(0x25, 0x2a, 0x33),
    control: Color32::from_rgb(0x22, 0x27, 0x30),
    hover: Color32::from_rgb(0x1d, 0x22, 0x2a),
    text: Color32::from_rgb(0xe8, 0xea, 0xef),
    weak: Color32::from_rgb(0x8b, 0x92, 0x9d),
    accent: Color32::from_rgb(0x3f, 0xb9, 0x50),
    warn: Color32::from_rgb(0xd8, 0xa1, 0x2c),
    danger: Color32::from_rgb(0xf0, 0x6a, 0x5f),
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf5, 0xf6, 0xf8),
    sidebar: Color32::from_rgb(0xec, 0xee, 0xf2),
    card: Color32::WHITE,
    border: Color32::from_rgb(0xe1, 0xe4, 0xe9),
    control: Color32::from_rgb(0xe6, 0xe9, 0xee),
    hover: Color32::from_rgb(0xe2, 0xe5, 0xea),
    text: Color32::from_rgb(0x15, 0x17, 0x1c),
    weak: Color32::from_rgb(0x66, 0x6d, 0x78),
    // Darker than the dark theme's, to keep its contrast on white.
    accent: Color32::from_rgb(0x1a, 0x8a, 0x3a),
    warn: Color32::from_rgb(0xb2, 0x6b, 0x00),
    danger: Color32::from_rgb(0xc9, 0x37, 0x2c),
};

pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

/// Inter for text, a semibold cut for titles, and both themes' visuals.
pub fn install(ctx: &egui::Context) {
    let mark = crate::icon::image(64);
    let pixels = mark.pixels().map(|p| Color32::from_white_alpha(p[3])).collect();
    let texture = ctx.load_texture("yunta-logo", egui::ColorImage::new([64, 64], pixels), egui::TextureOptions::LINEAR);
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("yunta-logo"), texture));
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("Inter".into(), Arc::new(FontData::from_static(include_bytes!("../assets/Inter-Regular.ttf"))));
    fonts.font_data.insert("Inter-SemiBold".into(), Arc::new(FontData::from_static(include_bytes!("../assets/Inter-SemiBold.ttf"))));
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter".into());
    fonts.families.insert(FontFamily::Name("semibold".into()), [vec!["Inter-SemiBold".to_string()], fallback].concat());
    ctx.set_fonts(fonts);
    for (theme, p) in [(Theme::Dark, &DARK), (Theme::Light, &LIGHT)] {
        ctx.set_visuals_of(theme, visuals(theme == Theme::Dark, p));
    }
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(10.0, 8.0);
        s.spacing.button_padding = Vec2::new(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.slider_width = 220.0;
        for (text, size) in [(TextStyle::Heading, 26.0), (TextStyle::Body, 14.0), (TextStyle::Button, 14.0), (TextStyle::Small, 12.0)] {
            if let Some(font) = s.text_styles.get_mut(&text) {
                font.size = size;
            }
        }
        if let Some(heading) = s.text_styles.get_mut(&TextStyle::Heading) {
            heading.family = semibold();
        }
    });
}

fn visuals(dark: bool, p: &Palette) -> Visuals {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.faint_bg_color = p.card;
    v.extreme_bg_color = if dark { Color32::from_rgb(0x0b, 0x0d, 0x10) } else { Color32::WHITE };
    v.window_stroke = Stroke::new(1.0, p.border);
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.danger;
    v.warn_fg_color = p.warn;
    v.weak_text_color = Some(p.weak);
    v.override_text_color = None;
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.bg_fill = p.card;
    for (state, fill) in [(&mut w.inactive, p.control), (&mut w.hovered, p.hover), (&mut w.active, p.hover), (&mut w.open, p.control)] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, p.border);
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.corner_radius = CornerRadius::same(CONTROL_RADIUS);
    }
    w.hovered.bg_stroke = Stroke::new(1.0, p.weak.gamma_multiply(0.6));
    w.hovered.expansion = 0.0;
    w.active.expansion = 0.0;
    v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if dark { 90 } else { 28 }) };
    v
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Text in the semibold cut.
pub fn bold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(semibold())
}

/// The page's title and the line under it, fading in with the page.
pub fn page_title(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.heading(title);
    ui.add_space(2.0);
    ui.label(RichText::new(detail).color(palette(ui).weak));
    ui.add_space(18.0);
}

/// A small caps-style label over a group of cards.
pub fn section(ui: &mut egui::Ui, label: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(label.to_uppercase()).size(11.0).color(palette(ui).weak).family(semibold()));
    ui.add_space(2.0);
}

pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let p = palette(ui);
    let shadow =
        Shadow { offset: [0, 1], blur: 3, spread: 0, color: Color32::from_black_alpha(if ui.visuals().dark_mode { 40 } else { 12 }) };
    egui::Frame::new().fill(p.card).stroke(Stroke::new(1.0, p.border)).corner_radius(CARD_RADIUS).inner_margin(18.0).shadow(shadow).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        },
    );
    ui.add_space(12.0);
}

/// A setting: its name with the control on the right, and a line about it underneath.
pub fn row(ui: &mut egui::Ui, title: &str, detail: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(bold(title).size(14.5));
        ui.with_layout(Flow::right_to_left(Align::Center), control);
    });
    ui.label(RichText::new(detail).size(12.5).color(palette(ui).weak));
}

/// A row with a switch. True when the switch was flipped.
pub fn toggle_row(ui: &mut egui::Ui, title: &str, detail: &str, on: &mut bool) -> bool {
    let mut changed = false;
    row(ui, title, detail, |ui| changed = toggle(ui, on, title).changed());
    changed
}

/// A divider between rows inside a card.
pub fn divider(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, palette(ui).border));
    ui.add_space(8.0);
}

pub fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(42.0, 24.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, *on, label));
    let p = palette(ui);
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let off = if resp.hovered() { p.hover.gamma_multiply(1.4) } else { p.control };
    let r = rect.height() / 2.0;
    let track = lerp_color(off, p.accent, t);
    ui.painter().rect(rect, r, track, Stroke::new(1.0, lerp_color(p.border, p.accent, t)), StrokeKind::Inside);
    let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
    let knob = Pos2::new(x, rect.center().y);
    ui.painter().circle_filled(knob + Vec2::new(0.0, 1.0), r - 3.0, Color32::from_black_alpha(50));
    ui.painter().circle_filled(knob, r - 3.0, Color32::WHITE);
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(3.0), r + 3.0, Stroke::new(2.0, p.accent.gamma_multiply(0.6)), StrokeKind::Outside);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A row of choices in one rounded track, the chosen one raised. True when the choice changed.
pub fn segmented(ui: &mut egui::Ui, chosen: &mut usize, options: &[&str]) -> bool {
    let p = palette(ui);
    let font = FontId::new(13.0, semibold());
    let widths: Vec<f32> =
        options.iter().map(|o| ui.painter().layout_no_wrap(o.to_string(), font.clone(), p.text).size().x + 26.0).collect();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(widths.iter().sum::<f32>() + 6.0, 32.0), Sense::hover());
    ui.painter().rect(rect, CONTROL_RADIUS, p.control, Stroke::new(1.0, p.border), StrokeKind::Inside);
    let mut changed = false;
    let mut x = rect.left() + 3.0;
    for (i, (option, w)) in options.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 3.0), Vec2::new(*w, rect.height() - 6.0));
        let resp = ui.interact(r, ui.id().with(("segment", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, *chosen == i, *option));
        if resp.clicked() && *chosen != i {
            *chosen = i;
            changed = true;
        }
        if *chosen == i {
            ui.painter().rect_filled(r.translate(Vec2::new(0.0, 1.0)), CONTROL_RADIUS - 2, Color32::from_black_alpha(30));
            ui.painter().rect_filled(r, CONTROL_RADIUS - 2, p.card);
        }
        let color = if *chosen == i || resp.hovered() { p.text } else { p.weak };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *option, font.clone(), color);
        x += w;
    }
    changed
}

/// The filled button for the one thing a card is for.
pub fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = palette(ui);
    ui.add(egui::Button::new(bold(text).color(Color32::WHITE)).fill(p.accent).stroke(Stroke::NONE).min_size(Vec2::new(0.0, 34.0)))
        .on_hover_cursor(CursorIcon::PointingHand)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    egui::lerp(egui::Rgba::from(a)..=egui::Rgba::from(b), t).into()
}

/// The Yoke brand mark, separate from the navigation's functional icons.
pub fn logo(painter: &egui::Painter, rect: Rect, color: Color32) {
    let texture = painter.ctx().data(|data| data.get_temp::<egui::TextureHandle>(egui::Id::new("yunta-logo"))).expect("installed logo");
    painter.image(texture.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), color);
}

#[derive(Clone, Copy)]
pub enum Icon {
    Overview,
    Arrangement,
    Keyboard,
    Pairing,
    Connection,
    About,
}

/// A 16-point line icon centred in `rect`.
pub fn icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = Stroke::new(1.5, color);
    let at = |x: f32, y: f32| c + Vec2::new(x, y);
    match icon {
        Icon::Overview => {
            for (x, y) in [(-4.0, -4.0), (4.0, -4.0), (-4.0, 4.0), (4.0, 4.0)] {
                painter.rect_stroke(Rect::from_center_size(at(x, y), Vec2::splat(6.0)), 1.5, s, StrokeKind::Middle);
            }
        }
        Icon::Arrangement => {
            for x in [-4.5, 4.5] {
                painter.rect_stroke(Rect::from_center_size(at(x, -1.5), Vec2::new(7.0, 6.0)), 1.0, s, StrokeKind::Middle);
                painter.line_segment([at(x, 1.5), at(x, 4.5)], s);
            }
            painter.line_segment([at(-6.5, 5.5), at(6.5, 5.5)], s);
        }
        Icon::Keyboard => {
            painter.rect_stroke(Rect::from_center_size(c, Vec2::new(15.0, 10.0)), 2.0, s, StrokeKind::Middle);
            for x in [-4.0, 0.0, 4.0] {
                painter.circle_filled(at(x, -1.5), 0.9, color);
            }
            painter.line_segment([at(-3.5, 2.0), at(3.5, 2.0)], s);
        }
        Icon::About => {
            painter.circle_stroke(c, 6.5, s);
            painter.circle_filled(at(0.0, -3.0), 0.9, color);
            painter.line_segment([at(0.0, -0.5), at(0.0, 3.5)], s);
        }
        Icon::Pairing => {
            for x in [-3.0, 3.0] {
                painter.rect_stroke(Rect::from_center_size(at(x, 0.0), Vec2::new(9.0, 6.0)), 3.0, s, StrokeKind::Middle);
            }
        }
        Icon::Connection => {
            let shield = vec![at(0.0, -7.0), at(6.0, -4.5), at(5.0, 2.5), at(0.0, 7.0), at(-5.0, 2.5), at(-6.0, -4.5)];
            painter.add(Shape::closed_line(shield, s));
            painter.add(Shape::line(vec![at(-2.5, 0.0), at(-0.5, 2.0), at(3.0, -2.0)], s));
        }
    }
}

/// A sidebar entry: icon, label, and an accent bar when it is the current page.
pub fn nav_item(ui: &mut egui::Ui, selected: bool, which: Icon, text: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, text));
    let p = palette(ui);
    let t = ui.ctx().animate_bool_with_time(resp.id, selected, 0.15);
    let fill = if selected || t > 0.0 {
        lerp_color(if resp.hovered() { p.hover } else { p.sidebar }, p.accent.gamma_multiply(0.16), t)
    } else if resp.hovered() {
        p.hover
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CONTROL_RADIUS, fill);
    if t > 0.0 {
        let bar = Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 9.0), Vec2::new(3.0, (rect.height() - 18.0) * t));
        painter.rect_filled(bar, 2.0, p.accent);
    }
    let color = if selected { p.text } else { lerp_color(p.weak, p.text, if resp.hovered() { 0.6 } else { 0.0 }) };
    icon(
        painter,
        Rect::from_center_size(rect.left_center() + Vec2::new(22.0, 0.0), Vec2::splat(16.0)),
        which,
        if selected { p.accent } else { color },
    );
    let font = if selected { FontId::new(14.0, semibold()) } else { FontId::proportional(14.0) };
    painter.text(rect.left_center() + Vec2::new(40.0, 0.0), Align2::LEFT_CENTER, text, font, color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A rounded label with a dot: the link's state at a glance.
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let font = FontId::new(12.0, semibold());
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = Vec2::new(galley.size().x + 28.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 12.0, color.gamma_multiply(0.14));
    ui.painter().circle_filled(rect.left_center() + Vec2::new(12.0, 0.0), 3.5, color);
    ui.painter().galley(rect.left_center() + Vec2::new(20.0, -galley.size().y / 2.0), galley, color);
}

/// Which of the two computers has the keyboard and mouse.
#[derive(Clone, Copy, PartialEq)]
pub enum Holder {
    Here,
    There,
}

/// What the drawing of the two computers shows.
#[derive(Clone, Copy, PartialEq)]
pub enum Scene {
    /// Linked: pulses run towards the computer with the keyboard and mouse.
    Linked(Holder),
    /// Paired, but the other one is not there: the link is broken.
    Offline,
    /// As offline, with the other one stirring while a wake-up goes out.
    Waking,
    /// Pairing mode: this computer calls out, and a space waits for the other.
    Searching,
    /// Not paired: this computer, and a space for the other.
    Alone,
    /// Yunta is not running here.
    Stopped,
}

/// The two computers side by side, and how they are linked.
pub fn two_screens(ui: &mut egui::Ui, here: &str, there: &str, peer_left: bool, scene: Scene) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 150.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let screen = Vec2::new(150.0, 92.0);
    let gap = (rect.width() * 0.5 - screen.x).clamp(60.0, 220.0);
    let left = Rect::from_center_size(rect.center() - Vec2::new(gap / 2.0 + screen.x / 2.0, 14.0), screen);
    let right = Rect::from_center_size(rect.center() + Vec2::new(gap / 2.0 + screen.x / 2.0, -14.0), screen);
    let (ours, theirs) = if peer_left { (right, left) } else { (left, right) };
    let time = ui.input(|i| i.time) as f32;
    let (a, b) = (Pos2::new(left.right() + 10.0, left.center().y), Pos2::new(right.left() - 10.0, right.center().y));

    match scene {
        Scene::Linked(holder) => {
            painter.line_segment([a, b], Stroke::new(2.0, p.accent.gamma_multiply(0.35)));
            // Three pulses running towards the holder.
            let towards_right = (holder == Holder::Here) != peer_left;
            for k in 0..3 {
                let t = ((time * 0.6 + k as f32 / 3.0) % 1.0).powf(1.2);
                let t = if towards_right { t } else { 1.0 - t };
                let at = a + (b - a) * t;
                let fade = (1.0 - (2.0 * t - 1.0).abs()).powf(0.6);
                painter.circle_filled(at, 7.0, p.accent.gamma_multiply(0.18 * fade));
                painter.circle_filled(at, 3.0, p.accent.gamma_multiply(fade));
            }
        }
        Scene::Offline | Scene::Waking | Scene::Stopped => {
            // Two ends of a cut cable, with a gap between.
            let mid = a + (b - a) * 0.5;
            let color = p.weak.gamma_multiply(0.6);
            let stroke = Stroke::new(2.0, color);
            painter.line_segment([a, mid - Vec2::new(12.0, 0.0)], stroke);
            painter.line_segment([mid + Vec2::new(12.0, 0.0), b], stroke);
            for side in [-1.0, 1.0] {
                let end = mid + Vec2::new(side * 12.0, 0.0);
                painter.line_segment([end + Vec2::new(side * -3.0, -6.0), end + Vec2::new(side * 3.0, 6.0)], stroke);
            }
        }
        Scene::Searching => {
            // Rings going out from this computer, towards where the other one will be.
            let c = Pos2::new(if peer_left { ours.left() } else { ours.right() }, ours.center().y);
            for k in 0..3 {
                let t = (time * 0.5 + k as f32 / 3.0) % 1.0;
                painter.circle_stroke(c, 10.0 + t * gap * 0.7, Stroke::new(2.0, p.accent.gamma_multiply(0.5 * (1.0 - t))));
            }
        }
        Scene::Alone => {}
    }

    let placeholder = matches!(scene, Scene::Searching | Scene::Alone);
    let here_active = matches!(scene, Scene::Linked(Holder::Here));
    let there_active = matches!(scene, Scene::Linked(Holder::There));
    let dim = matches!(scene, Scene::Offline | Scene::Waking | Scene::Stopped);
    monitor(&painter, p, ours, here, here_active, scene == Scene::Stopped, time);
    if placeholder {
        // A dashed outline where the other computer will stand.
        let r = theirs;
        let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        for shape in Shape::dashed_line(&pts, Stroke::new(1.5, p.weak.gamma_multiply(0.6)), 6.0, 5.0) {
            painter.add(shape);
        }
        painter.text(r.center(), Align2::CENTER_CENTER, "+", FontId::new(26.0, semibold()), p.weak.gamma_multiply(0.8));
        painter.text(r.center_bottom() + Vec2::new(0.0, 30.0), Align2::CENTER_CENTER, there, FontId::new(13.0, semibold()), p.weak);
    } else {
        let stirring = scene == Scene::Waking;
        if dim || stirring {
            let breathe = if stirring { 0.45 + 0.25 * (time * 3.0).sin() } else { 0.45 };
            let mut layer = painter.clone();
            layer.set_opacity(breathe);
            monitor(&layer, p, theirs, there, false, true, time);
        } else {
            monitor(&painter, p, theirs, there, there_active, false, time);
        }
    }
}

/// One computer screen on its stand, glowing and with a pointer on it when it has the input.
fn monitor(painter: &egui::Painter, p: &Palette, r: Rect, name: &str, active: bool, grey: bool, time: f32) {
    if active {
        let breathe = 0.75 + 0.25 * (time * 2.0).sin();
        for (grow, alpha) in [(10.0, 0.06), (5.0, 0.12)] {
            painter.rect_filled(r.expand(grow), 10.0 + grow, p.accent.gamma_multiply(alpha * breathe));
        }
    }
    let border = if active { p.accent } else { p.frame() };
    painter.rect(r, 8.0, p.control, Stroke::new(1.5, border), StrokeKind::Inside);
    // The glass: a soft sheen across the top.
    let glass = r.shrink(6.0);
    painter.rect_filled(glass, 4.0, if active { p.accent.gamma_multiply(0.10) } else { p.bg.gamma_multiply(0.5) });
    painter.rect_filled(
        Rect::from_min_max(glass.min, Pos2::new(glass.max.x, glass.min.y + glass.height() * 0.4)),
        4.0,
        Color32::from_white_alpha(6),
    );
    let foot = r.center_bottom();
    painter.rect_filled(Rect::from_center_size(foot + Vec2::new(0.0, 7.0), Vec2::new(10.0, 12.0)), 2.0, p.frame());
    painter.rect_filled(Rect::from_center_size(foot + Vec2::new(0.0, 13.0), Vec2::new(44.0, 4.0)), 2.0, p.frame());
    if active {
        pointer(painter, glass.center() + Vec2::new(-4.0, -8.0), p.text, p.bg);
    }
    let color = if active {
        p.text
    } else if grey {
        p.weak.gamma_multiply(0.8)
    } else {
        p.weak
    };
    painter.text(foot + Vec2::new(0.0, 30.0), Align2::CENTER_CENTER, name, FontId::new(13.0, semibold()), color);
}

/// A mouse pointer, tip at `tip`.
fn pointer(painter: &egui::Painter, tip: Pos2, color: Color32, outline: Color32) {
    let at = |x: f32, y: f32| tip + Vec2::new(x, y);
    // Filled as two convex parts, the head and the tail, then outlined as one.
    painter.add(Shape::convex_polygon(vec![at(0.0, 0.0), at(0.0, 15.0), at(11.0, 10.5)], color, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![at(4.0, 11.5), at(7.0, 17.5), at(9.5, 16.5), at(6.5, 10.5)], color, Stroke::NONE));
    let arrow = vec![at(0.0, 0.0), at(0.0, 15.0), at(4.0, 11.5), at(7.0, 17.5), at(9.5, 16.5), at(6.5, 10.5), at(11.0, 10.5)];
    painter.add(Shape::closed_line(arrow, Stroke::new(1.2, outline)));
}

/// A pairing code, one digit to a tile.
pub fn code_digits(ui: &mut egui::Ui, code: &str) {
    let p = palette(ui);
    let tile = Vec2::new(46.0, 58.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (i, digit) in code.chars().enumerate() {
            if i == 3 {
                ui.add_space(10.0);
            }
            let (rect, _) = ui.allocate_exact_size(tile, Sense::hover());
            ui.painter().rect(rect, 10.0, p.control, Stroke::new(1.0, p.border), StrokeKind::Inside);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, digit, FontId::new(30.0, semibold()), p.text);
        }
    });
}

/// A thin bar, full at 1.
pub fn progress(ui: &mut egui::Ui, fraction: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, p.control);
    let done = Rect::from_min_size(rect.min, Vec2::new(rect.width() * fraction.clamp(0.0, 1.0), rect.height()));
    ui.painter().rect_filled(done, 2.0, p.accent);
}

/// A tinted strip with a message: good news in the accent, bad in red.
pub fn banner(ui: &mut egui::Ui, text: &str, ok: bool) {
    let p = palette(ui);
    let color = if ok { p.accent } else { p.danger };
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.4)))
        .corner_radius(CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(color));
        });
    ui.add_space(12.0);
}

/// A round badge with the first letter of a computer's name.
pub fn avatar(ui: &mut egui::Ui, name: &str) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 16.0, p.accent.gamma_multiply(0.16));
    let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, letter, FontId::new(14.0, semibold()), p.accent);
}
