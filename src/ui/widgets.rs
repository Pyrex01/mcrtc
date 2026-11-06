//! Controls painted the way Minecraft paints its menus. One point is one Minecraft GUI pixel, so
//! the sizes here are the game's: 20 tall buttons, 200 wide columns, 9 point text.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    pos2, vec2, Align, Align2, Color32, Context, CornerRadius, CursorIcon, FontData,
    FontDefinitions, FontFamily, FontId, Frame, Galley, Id, LayerId, Margin, Order, Painter, Pos2,
    Rect, Response, Role, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextBuffer, TextEdit,
    TextStyle, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2, WidgetInfo,
};
use eframe::epaint::text::{FontInsert, FontPriority, FontTweak, InsertFontFamily};
use eframe::epaint::TextShape;

use super::art;

const FONT: &[u8] = include_bytes!("../../assets/ui/monocraft.ttf");
/// The font is drawn on a grid of nine cells to the em, so this puts one cell on one GUI pixel.
const FONT_SIZE: f32 = 9.0;
/// Where the top of a capital sits below the top of a laid out line.
const CAP_TOP: f32 = 3.0;

pub const BUTTON_HEIGHT: f32 = 20.0;
pub const COLUMN: f32 = 200.0;
pub const GAP: f32 = 4.0;
const SLOT_HEIGHT: f32 = 36.0;
/// A background tile covers this many GUI pixels, so each of its texels is two of them.
const TILE: f32 = 32.0;

pub const WHITE: Color32 = Color32::WHITE;
pub const MUTED: Color32 = Color32::from_rgb(0xa0, 0xa0, 0xa0);
pub const YELLOW: Color32 = Color32::from_rgb(0xff, 0xff, 0x55);
pub const RED: Color32 = Color32::from_rgb(0xff, 0x55, 0x55);
const GREEN: Color32 = Color32::from_rgb(0x55, 0xff, 0x55);
const FIELD_TEXT: Color32 = Color32::from_rgb(0xe0, 0xe0, 0xe0);
const TOOLTIP_FILL: Color32 = Color32::from_rgb(0x10, 0x00, 0x10);
const TOOLTIP_BORDER: Color32 = Color32::from_rgb(0x28, 0x00, 0x7f);

fn font() -> FontId {
    FontId::new(FONT_SIZE, FontFamily::Proportional)
}

/// Text shadows are a quarter of the text's brightness, one pixel down and right.
fn shadow_of(color: Color32) -> Color32 {
    let [r, g, b, _] = color.to_array();
    Color32::from_rgb(r / 4, g / 4, b / 4)
}

pub fn install_style(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    for family in fonts.families.values_mut() {
        family.clear();
    }
    ctx.set_fonts(fonts);
    let tweak = FontTweak {
        hinting: Some(false),
        ..FontTweak::default()
    };
    ctx.add_font(FontInsert::new(
        "monocraft",
        FontData::from_static(FONT).tweak(tweak),
        [FontFamily::Proportional, FontFamily::Monospace]
            .map(|family| InsertFontFamily {
                family,
                priority: FontPriority::Highest,
            })
            .to_vec(),
    ));

    ctx.options_mut(|options| options.zoom_with_keyboard = false);
    ctx.all_styles_mut(|style| {
        for text_style in [
            TextStyle::Small,
            TextStyle::Body,
            TextStyle::Button,
            TextStyle::Heading,
            TextStyle::Monospace,
        ] {
            style.text_styles.insert(text_style, font());
        }
        style.interaction.selectable_labels = false;
        style.spacing.item_spacing = vec2(GAP, GAP);
        style.spacing.scroll.floating = false;
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.bar_inner_margin = 0.0;
        style.spacing.scroll.bar_outer_margin = 0.0;

        let visuals = &mut style.visuals;
        visuals.dark_mode = true;
        visuals.override_text_color = Some(FIELD_TEXT);
        visuals.selection.bg_fill = Color32::from_rgb(0, 0, 0xaa);
        visuals.selection.stroke = Stroke::new(1.0, WHITE);
        // The fields paint Minecraft's cursor themselves.
        visuals.text_cursor.stroke = Stroke::NONE;
        visuals.text_cursor.on_duration = 0.3;
        visuals.text_cursor.off_duration = 0.3;
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::ZERO;
            widget.bg_fill = Color32::from_gray(0x80);
            widget.weak_bg_fill = Color32::from_gray(0x80);
            widget.fg_stroke = Stroke::new(1.0, Color32::from_gray(0xc0));
            widget.bg_stroke = Stroke::NONE;
            widget.expansion = 0.0;
        }
        visuals.extreme_bg_color = Color32::BLACK;
    });
}

/// Textures live on the GPU, so they belong to the window, not to the art module.
pub struct Theme {
    earth: TextureHandle,
    stone: TextureHandle,
    logo: TextureHandle,
    portraits: RefCell<HashMap<String, TextureHandle>>,
}

impl Theme {
    pub fn new(ctx: &Context) -> Self {
        Self {
            earth: ctx.load_texture("earth", art::earth(), TextureOptions::NEAREST_REPEAT),
            stone: ctx.load_texture("stone", art::stone(), TextureOptions::NEAREST_REPEAT),
            logo: ctx.load_texture("logo", art::logo(), TextureOptions::NEAREST),
            portraits: RefCell::default(),
        }
    }

    fn portrait(&self, ctx: &Context, name: &str) -> TextureHandle {
        self.portraits
            .borrow_mut()
            .entry(name.to_owned())
            .or_insert_with(|| {
                let image = art::portrait(name);
                ctx.load_texture(format!("portrait-{name}"), image, TextureOptions::NEAREST)
            })
            .clone()
    }

    fn tile(
        &self,
        painter: &Painter,
        texture: &TextureHandle,
        rect: Rect,
        size: f32,
        tint: Color32,
    ) {
        let uv = Rect::from_min_max(
            (rect.min.to_vec2() / size).to_pos2(),
            (rect.max.to_vec2() / size).to_pos2(),
        );
        painter.image(texture.id(), rect, uv, tint);
    }

    pub fn background(&self, ui: &Ui, rect: Rect) {
        self.tile(ui.painter(), &self.earth, rect, TILE, Color32::WHITE);
    }

    pub fn logo(&self, ui: &Ui, top_center: Pos2) -> Rect {
        let size = self.logo.size_vec2();
        let rect = Rect::from_min_size(top_center - vec2((size.x / 2.0).round(), 0.0), size);
        let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
        ui.painter().image(self.logo.id(), rect, uv, Color32::WHITE);
        rect
    }
}

fn layout(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    wrap: f32,
    halign: Align,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, wrap);
    job.halign = halign;
    ui.painter().layout_job(job)
}

fn layout_line(ui: &Ui, text: &str, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font(), color);
    job.wrap = TextWrapping::truncate_at_width(max_width);
    ui.painter().layout_job(job)
}

fn paint_text(painter: &Painter, pos: Pos2, galley: Arc<Galley>, color: Color32) {
    let shadow = shadow_of(color);
    painter.galley_with_override_text_color(pos + vec2(1.0, 1.0), galley.clone(), shadow);
    painter.galley_with_override_text_color(pos, galley, color);
}

/// Text drawn straight onto the screen. `pos` is where the top of its capitals go.
pub fn text_at(ui: &Ui, painter: &Painter, pos: Pos2, align: Align2, text: &str, color: Color32) {
    let galley = layout_line(ui, text, color, f32::INFINITY);
    let left = align.x().to_factor() * galley.size().x;
    let pos = pos2((pos.x - left).round(), pos.y - CAP_TOP);
    paint_text(painter, pos, galley, color);
}

/// A wrapped block of text taking the full width of the column.
pub fn paragraph(ui: &mut Ui, text: &str, color: Color32, halign: Align) -> Response {
    paragraph_with(ui, text, font(), color, halign)
}

fn paragraph_with(
    ui: &mut Ui,
    text: &str,
    font: FontId,
    color: Color32,
    halign: Align,
) -> Response {
    let width = ui.available_width();
    let galley = layout(ui, text, font, color, width, halign);
    let (rect, response) = ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
    let x = rect.left() + halign.to_factor() * width;
    paint_text(ui.painter(), pos2(x, rect.top()), galley, color);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    response
}

/// Text twice the normal size, like the address of a server.
pub fn big_paragraph(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let big = FontId::new(FONT_SIZE * 2.0, FontFamily::Proportional);
    paragraph_with(ui, text, big, color, Align::Center)
}

pub fn gap(ui: &mut Ui, height: f32) {
    ui.add_space(height);
}

pub fn title(ui: &Ui, top_center: Pos2, text: &str) {
    text_at(
        ui,
        ui.painter(),
        top_center,
        Align2::CENTER_TOP,
        text,
        WHITE,
    );
}

/// The darker strip list screens keep between their title and their buttons.
pub fn list_band(ui: &Ui, rect: Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(160));
    let line = |y: f32, color: Color32| {
        let row = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), 1.0));
        painter.rect_filled(row, CornerRadius::ZERO, color);
    };
    line(rect.top() - 1.0, Color32::from_white_alpha(36));
    line(rect.top(), Color32::BLACK);
    line(rect.bottom() - 1.0, Color32::BLACK);
    line(rect.bottom(), Color32::from_white_alpha(36));
}

/// Draws the frame just inside `rect`, the way the game outlines its controls.
fn outline(painter: &Painter, rect: Rect, color: Color32) {
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
}

pub fn button(ui: &mut Ui, theme: &Theme, text: &str, width: f32, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(width, BUTTON_HEIGHT), sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, text));
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter();
    let lit = enabled && (response.hovered() || response.has_focus());
    let color = if enabled { WHITE } else { MUTED };
    if enabled {
        theme.tile(painter, &theme.stone, rect, 16.0, Color32::WHITE);
        let inner = rect.shrink(1.0);
        let edge =
            |rect: Rect, color: Color32| painter.rect_filled(rect, CornerRadius::ZERO, color);
        let (light, dark) = (Color32::from_white_alpha(87), Color32::from_black_alpha(87));
        edge(
            Rect::from_min_size(inner.min, vec2(inner.width(), 1.0)),
            light,
        );
        edge(
            Rect::from_min_size(inner.min, vec2(1.0, inner.height())),
            light,
        );
        let bottom = pos2(inner.left(), inner.bottom() - 2.0);
        edge(Rect::from_min_size(bottom, vec2(inner.width(), 2.0)), dark);
        let right = pos2(inner.right() - 1.0, inner.top());
        edge(Rect::from_min_size(right, vec2(1.0, inner.height())), dark);
    } else {
        painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(0x2c));
    }
    outline(painter, rect, if lit { WHITE } else { Color32::BLACK });

    let galley = layout_line(ui, text, color, width - 8.0);
    let x = (rect.center().x - galley.size().x / 2.0).round();
    let y = rect.top() + (BUTTON_HEIGHT - 8.0) / 2.0 - CAP_TOP;
    paint_text(painter, pos2(x, y), galley, color);

    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// What `TextEdit` leaves for the fields to paint beneath and on top of it.
struct Field {
    id: Id,
    background: eframe::egui::layers::ShapeIdx,
    shadow: eframe::egui::layers::ShapeIdx,
}

impl Field {
    fn reserve(ui: &Ui, id: Id) -> Self {
        Self {
            id,
            background: ui.painter().add(Shape::Noop),
            shadow: ui.painter().add(Shape::Noop),
        }
    }

    fn finish(
        self,
        ui: &Ui,
        rect: Rect,
        clip: Rect,
        output: &eframe::egui::text_edit::TextEditOutput,
    ) {
        let focused = ui.memory(|memory| memory.has_focus(self.id));
        let border = if focused { WHITE } else { MUTED };
        let background = vec![
            Shape::rect_filled(rect, CornerRadius::ZERO, Color32::BLACK),
            Shape::rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, border),
                StrokeKind::Inside,
            ),
        ];
        ui.painter().set(self.background, Shape::Vec(background));

        let painter = ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()));
        let shadow = shadow_of(FIELD_TEXT);
        let text = TextShape::new(
            output.galley_pos + vec2(1.0, 1.0),
            output.galley.clone(),
            shadow,
        )
        .with_override_text_color(shadow);
        painter.set(self.shadow, text);

        let blink_on = ((ui.input(|input| input.time) / 0.3) as u64).is_multiple_of(2);
        if let (true, true, Some(range)) = (focused, blink_on, output.cursor_range) {
            let cursor = output.galley.pos_from_cursor(range.primary);
            let at = output.galley_pos + cursor.min.to_vec2();
            let at_end = range.primary.index.0 >= output.galley.text().chars().count();
            let mark = if at_end {
                Rect::from_min_size(at + vec2(0.0, CAP_TOP + 8.0), vec2(5.0, 1.0))
            } else {
                Rect::from_min_size(at + vec2(-1.0, CAP_TOP - 1.0), vec2(1.0, 9.0))
            };
            painter.rect_filled(mark, CornerRadius::ZERO, FIELD_TEXT);
        }
    }
}

fn hint(text: &str) -> eframe::egui::RichText {
    eframe::egui::RichText::new(text)
        .font(font())
        .color(Color32::from_gray(0x70))
}

/// A labelled one-line text box. `tune` adds whatever this particular field needs.
pub fn field<'t>(
    ui: &mut Ui,
    label: &str,
    text: &'t mut String,
    placeholder: &str,
    tune: impl FnOnce(TextEdit<'t>) -> TextEdit<'t>,
) -> Response {
    let (width, caption) = caption(ui, label);
    let id = ui.make_persistent_id(label);
    let reserved = Field::reserve(ui, id);
    let edit = TextEdit::singleline(text)
        .id(id)
        .frame(Frame::NONE.inner_margin(Margin::same(4)))
        .desired_width(width)
        .text_color(FIELD_TEXT)
        .font(font())
        .hint_text(hint(placeholder));
    let output = tune(edit).show(ui);
    let rect = output.response.response.rect;
    reserved.finish(ui, rect, rect.shrink(1.0), &output);
    output.response.response.clone().labelled_by(caption.id)
}

fn caption(ui: &mut Ui, label: &str) -> (f32, Response) {
    let width = ui.available_width();
    let response = paragraph(ui, label, MUTED, Align::Min);
    (width, response)
}

/// A labelled scrolling box for the long strings. Read only when given a `&str`.
pub fn text_box(ui: &mut Ui, label: &str, height: f32, text: &mut dyn TextBuffer) -> Response {
    let (width, caption) = caption(ui, label);
    let id = ui.make_persistent_id(label);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let background = ui.painter().add(Shape::Noop);

    let mut inner = ui.new_child(UiBuilder::new().max_rect(rect.shrink(1.0)));
    // The strings have no spaces, so they fill every line instead of breaking at their dashes.
    let mut wrap_anywhere = |ui: &Ui, text: &dyn TextBuffer, width: f32| {
        let mut job = LayoutJob::simple(text.as_str().to_owned(), font(), FIELD_TEXT, width);
        job.wrap.break_anywhere = true;
        ui.painter().layout_job(job)
    };
    let output = ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink(false)
        .min_scrolled_height(0.0)
        .max_height(height - 2.0)
        .show(&mut inner, |ui| {
            let reserved = Field {
                id,
                background: ui.painter().add(Shape::Noop),
                shadow: ui.painter().add(Shape::Noop),
            };
            let output = TextEdit::multiline(text)
                .id(id)
                .frame(Frame::NONE.inner_margin(Margin::same(3)))
                .desired_width(f32::INFINITY)
                .min_size(vec2(0.0, height - 2.0))
                .text_color(FIELD_TEXT)
                .font(font())
                .layouter(&mut wrap_anywhere)
                .show(ui);
            (reserved, output)
        });
    let (reserved, output) = output.inner;
    let frame_slot = Field {
        background,
        ..reserved
    };
    frame_slot.finish(ui, rect, rect.shrink(1.0), &output);
    output.response.response.clone().labelled_by(caption.id)
}

/// How many of the five ping bars are lit.
fn signal(ping: Option<u64>) -> usize {
    match ping {
        None => 0,
        Some(0..80) => 5,
        Some(80..150) => 4,
        Some(150..300) => 3,
        Some(300..600) => 2,
        Some(_) => 1,
    }
}

fn ping_bars(ui: &Ui, right_top: Pos2, ping: Option<u64>) -> Rect {
    const HEIGHTS: [f32; 5] = [2.0, 3.0, 5.0, 6.0, 8.0];
    let lit = signal(ping);
    let color = match lit {
        4.. => GREEN,
        2.. => YELLOW,
        _ => RED,
    };
    let area = Rect::from_min_size(right_top - vec2(14.0, 0.0), vec2(14.0, 8.0));
    for (index, height) in HEIGHTS.into_iter().enumerate() {
        let x = area.left() + index as f32 * 3.0;
        let bar = Rect::from_min_max(
            pos2(x, area.bottom() - height),
            pos2(x + 2.0, area.bottom()),
        );
        let fill = if index < lit {
            color
        } else {
            Color32::from_gray(0x33)
        };
        ui.painter().rect_filled(
            bar.translate(vec2(1.0, 1.0)),
            0.0,
            Color32::from_black_alpha(128),
        );
        ui.painter().rect_filled(bar, 0.0, fill);
    }
    area
}

/// One line of a list, drawn like an entry in the server list.
pub fn list_entry(ui: &mut Ui, theme: &Theme, name: &str, detail: &str, ping: Option<u64>) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, SLOT_HEIGHT), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
    if !ui.is_rect_visible(rect) {
        return;
    }
    if response.hovered() {
        outline(ui.painter(), rect, Color32::from_gray(0x80));
    }

    let inner = rect.shrink(2.0);
    let portrait = theme.portrait(ui.ctx(), name);
    let face = Rect::from_min_size(inner.min, vec2(32.0, 32.0));
    let uv = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));
    ui.painter().image(portrait.id(), face, uv, Color32::WHITE);

    let left = face.right() + 3.0;
    let text_width = inner.right() - left;
    let top = inner.top() + 1.0 - CAP_TOP;
    let galley = layout_line(ui, name, WHITE, text_width - 20.0);
    paint_text(ui.painter(), pos2(left, top), galley, WHITE);
    let galley = layout_line(ui, detail, MUTED, text_width);
    paint_text(ui.painter(), pos2(left, top + 12.0), galley, MUTED);

    let bars = ping_bars(ui, inner.right_top() + vec2(-1.0, 1.0), ping);
    if ui.rect_contains_pointer(bars.expand(2.0)) {
        if let Some(pointer) = ui.input(|input| input.pointer.hover_pos()) {
            let label = ping.map_or("Measuring...".to_owned(), |ping| format!("{ping} ms"));
            tooltip(ui, pointer + vec2(10.0, -14.0), &label);
        }
    }
}

fn tooltip_box(painter: &Painter, rect: Rect, text: Arc<Galley>) {
    painter.rect_filled(rect, CornerRadius::ZERO, TOOLTIP_FILL);
    outline(painter, rect.shrink(1.0), TOOLTIP_BORDER);
    let pos = rect.min + vec2(5.0, 5.0 - CAP_TOP);
    paint_text(painter, pos, text, WHITE);
}

fn tooltip(ui: &Ui, pos: Pos2, text: &str) {
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Tooltip, Id::unique("tooltip")));
    let galley = layout_line(ui, text, WHITE, f32::INFINITY);
    let size = vec2(galley.size().x + 10.0, 18.0);
    tooltip_box(&painter, Rect::from_min_size(pos.round(), size), galley);
}

/// A notice in the top right corner. `visible` runs from 0 when hidden to 1 when fully out.
pub fn toast(ui: &Ui, screen: Rect, text: &str, visible: f32) {
    let painter = ui
        .ctx()
        .layer_painter(LayerId::new(Order::Foreground, Id::unique("toast")));
    let galley = layout_line(ui, text, WHITE, screen.width() - 18.0);
    let size = vec2(galley.size().x + 10.0, 18.0);
    let hidden = (1.0 - visible) * (size.x + 4.0);
    let min = pos2(
        screen.right() - size.x - 4.0 + hidden.round(),
        screen.top() + 4.0,
    );
    tooltip_box(&painter, Rect::from_min_size(min, size), galley);
}

/// The yellow text leaning over the logo, pulsing around `center`.
pub fn splash(ui: &Ui, center: Pos2, text: &str, scale: f32) {
    use eframe::egui::emath::TSTransform;

    let layer = LayerId::new(Order::Foreground, Id::unique("splash"));
    let painter = ui.ctx().layer_painter(layer);
    let angle = -20_f32.to_radians();
    let galley = layout_line(ui, text, YELLOW, f32::INFINITY);
    for (offset, color) in [(vec2(1.0, 1.0), shadow_of(YELLOW)), (Vec2::ZERO, YELLOW)] {
        let shape = TextShape::new(center + offset, galley.clone(), color)
            .with_override_text_color(color)
            .with_angle_and_anchor(angle, Align2::CENTER_CENTER);
        painter.add(shape);
    }
    let to_center = TSTransform::from_translation(center.to_vec2());
    let transform = to_center * TSTransform::from_scaling(scale) * to_center.inverse();
    ui.ctx().set_transform_layer(layer, transform);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadows_are_a_quarter_as_bright() {
        assert_eq!(shadow_of(YELLOW), Color32::from_rgb(0x3f, 0x3f, 0x15));
    }

    #[test]
    fn ping_lights_fewer_bars_as_it_gets_worse() {
        let bars = [None, Some(30), Some(80), Some(200), Some(400), Some(900)].map(signal);
        assert_eq!(bars, [0, 5, 4, 3, 2, 1]);
    }
}
