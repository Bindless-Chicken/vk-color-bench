use crate::color::{ColorAnalysisResult, PreparedAnalysis, nits_to_pq};

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum ExpandedView {
    LuminanceParade,
    RGBParade,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub(crate) enum OverlayMode {
    Clipping,
    Channels(Channel),
    Gamut,
    Luminance,
}

#[derive(Debug, PartialEq, Copy, Clone, Default)]
#[repr(u32)]
pub(crate) enum Channel {
    #[default]
    Red = 0,
    Green = 1,
    Blue = 2,
    Alpha = 3,
}

impl Channel {
    fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Alpha => "Alpha",
        }
    }
}

pub(crate) struct UiFrame {
    pub(crate) output: egui::FullOutput,
    pub(crate) primitives: Vec<egui::ClippedPrimitive>,
    pub(crate) overlay: Option<OverlayMode>,
    pub(crate) capture_pointer: bool,
    pub(crate) capture_keyboard: bool,
}

#[derive(Default)]
pub(crate) struct UiContext {
    context: egui::Context,

    frame_number: u64,

    expanded_view: Option<ExpandedView>,

    color_analysis: Option<ColorAnalysisResult>,
    pending_analysis: Option<ColorAnalysisResult>,

    luminance_parade_texture: Option<egui::TextureHandle>,
    color_parade_texture: Option<egui::TextureHandle>,

    active_overlay: Option<OverlayMode>,
    selected_channel: Channel,
    hdr_gamut_available: bool,
    restore_font_texture: bool,
}

impl UiContext {
    pub(crate) fn reset_analysis(&mut self) {
        self.color_analysis = None;
        self.pending_analysis = None;
        self.luminance_parade_texture = None;
        self.color_parade_texture = None;
        self.restore_font_texture = true;
    }

    pub(crate) fn set_hdr_gamut_available(&mut self, available: bool) {
        self.hdr_gamut_available = available;
        if !available
            && matches!(
                self.active_overlay,
                Some(OverlayMode::Gamut | OverlayMode::Luminance)
            )
        {
            self.active_overlay = None;
        }
    }

    pub(crate) fn set_color_analysis(&mut self, prepared: PreparedAnalysis) {
        update_texture(
            &self.context,
            &mut self.luminance_parade_texture,
            "luminance-parade",
            prepared.luminance,
        );
        update_texture(
            &self.context,
            &mut self.color_parade_texture,
            "color-parade",
            prepared.color,
        );
        self.pending_analysis = Some(prepared.analysis);
    }

    pub(crate) fn accepts_analysis(&self) -> bool {
        self.pending_analysis.is_none()
    }

    pub(crate) fn commit_analysis(&mut self, uploaded: &[egui::TextureId]) {
        if self
            .luminance_parade_texture
            .as_ref()
            .is_some_and(|texture| uploaded.contains(&texture.id()))
            && self
                .color_parade_texture
                .as_ref()
                .is_some_and(|texture| uploaded.contains(&texture.id()))
        {
            self.color_analysis = self.pending_analysis.take();
        }
    }

    pub(crate) fn prepare_frame(&mut self, size: [u32; 2], mut input: egui::RawInput) -> UiFrame {
        self.frame_number += 1;

        let screen_rect =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32));
        input.screen_rect = Some(screen_rect);
        let frame_number = self.frame_number;
        let expanded_view = &mut self.expanded_view;
        let color_analysis = self.color_analysis.as_ref();
        let luminance_parade_texture = self
            .luminance_parade_texture
            .as_ref()
            .map(egui::TextureHandle::id);
        let color_parade_texture = self
            .color_parade_texture
            .as_ref()
            .map(egui::TextureHandle::id);
        let mut output = self.context.run(input, |context| {
            egui::Window::new("Color Bench")
                .default_pos(egui::pos2(12.0, 12.0))
                .resizable(false)
                .show(context, |ui| {
                    ui.label(format!("Frame {frame_number}"));
                    ui.label(format!("{} x {}", size[0], size[1]));
                    if let Some(analysis) = &color_analysis {
                        ui.label(format!("Format: {:?}", analysis.pixel_format));
                        ui.label(format!(
                            "Analysis samples: {} x {}",
                            analysis.sample_size[0], analysis.sample_size[1]
                        ));
                        egui::CollapsingHeader::new("Histograms")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.label("Luminance");
                                histogram(
                                    ui,
                                    &[&analysis.luminance_histogram.data],
                                    &[egui::Color32::WHITE],
                                    analysis.luminance_histogram.maxima,
                                );

                                ui.label("Color");
                                let red: Vec<_> = analysis
                                    .color_histogram
                                    .data
                                    .iter()
                                    .map(|bins| bins[0])
                                    .collect();
                                let green: Vec<_> = analysis
                                    .color_histogram
                                    .data
                                    .iter()
                                    .map(|bins| bins[1])
                                    .collect();
                                let blue: Vec<_> = analysis
                                    .color_histogram
                                    .data
                                    .iter()
                                    .map(|bins| bins[2])
                                    .collect();
                                histogram(
                                    ui,
                                    &[&red, &green, &blue],
                                    &[
                                        egui::Color32::RED,
                                        egui::Color32::GREEN,
                                        egui::Color32::BLUE,
                                    ],
                                    analysis
                                        .color_histogram
                                        .maxima
                                        .into_iter()
                                        .max()
                                        .unwrap_or(0),
                                );
                            });

                        egui::CollapsingHeader::new("Parades")
                            .default_open(false)
                            .show(ui, |ui| {
                                Self::enum_checkbox(
                                    ui,
                                    expanded_view,
                                    ExpandedView::LuminanceParade,
                                    "Expanded Luminance Parade",
                                );
                                ui.label("Luminance");
                                if let Some(texture) = luminance_parade_texture {
                                    parade(ui, texture, analysis.luminance_is_pq);
                                }

                                Self::enum_checkbox(
                                    ui,
                                    expanded_view,
                                    ExpandedView::RGBParade,
                                    "Expanded RGB Parade",
                                );
                                ui.label("RGB");
                                if let Some(texture) = color_parade_texture {
                                    parade(ui, texture, false);
                                }
                            });

                        egui::CollapsingHeader::new("Overlays")
                            .default_open(false)
                            .show(ui, |ui| {
                                let label = match self.active_overlay {
                                    None => "None",
                                    Some(OverlayMode::Clipping) => "Clipping",
                                    Some(OverlayMode::Channels(_)) => "Channels",
                                    Some(OverlayMode::Gamut) => "Gamut",
                                    Some(OverlayMode::Luminance) => "Luminance",
                                };
                                egui::ComboBox::from_label("Overlay")
                                    .selected_text(label)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.active_overlay, None, "None");
                                        ui.selectable_value(
                                            &mut self.active_overlay,
                                            Some(OverlayMode::Clipping),
                                            "Clipping",
                                        );
                                        ui.selectable_value(
                                            &mut self.active_overlay,
                                            Some(OverlayMode::Channels(self.selected_channel)),
                                            "Channels",
                                        );
                                        ui.add_enabled_ui(self.hdr_gamut_available, |ui| {
                                            ui.selectable_value(
                                                &mut self.active_overlay,
                                                Some(OverlayMode::Luminance),
                                                "Luminance",
                                            );
                                            ui.selectable_value(
                                                &mut self.active_overlay,
                                                Some(OverlayMode::Gamut),
                                                "Gamut",
                                            );
                                        })
                                        .response
                                        .on_disabled_hover_text("Requires an HDR10 PQ swapchain");
                                    });
                                if self.active_overlay == Some(OverlayMode::Gamut) {
                                    ui.label("Red: Rec. 709, Green: DCI-P3, Blue: Rec. 2020");
                                    ui.label(
                                        "Smallest containing gamut, independent of brightness.",
                                    );
                                }
                                if self.active_overlay == Some(OverlayMode::Luminance) {
                                    luminance_legend(ui);
                                }
                                if let Some(OverlayMode::Channels(channel)) =
                                    &mut self.active_overlay
                                {
                                    egui::ComboBox::from_label("Channel")
                                        .selected_text(self.selected_channel.label())
                                        .show_ui(ui, |ui| {
                                            for option in [
                                                Channel::Red,
                                                Channel::Green,
                                                Channel::Blue,
                                                Channel::Alpha,
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.selected_channel,
                                                    option,
                                                    option.label(),
                                                );
                                            }
                                        });
                                    *channel = self.selected_channel;
                                    ui.label("Selected channel shown in grayscale.");
                                }
                            });

                        ui.collapsing("Stats", |ui| {
                            ui.label(format!(
                                "Analysis duration: {}us",
                                analysis.duration.as_micros()
                            ));
                        });
                    } else {
                        ui.label("Color analysis: waiting for readback...");
                    }
                });

            if let Some(expanded_view) = expanded_view {
                egui::Area::new(egui::Id::new("large_parade"))
                    .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(0.0, 0.0))
                    .show(context, |ui| {
                        let texture = match expanded_view {
                            ExpandedView::LuminanceParade => luminance_parade_texture,
                            ExpandedView::RGBParade => color_parade_texture,
                        };

                        if let Some(texture) = texture {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(size[0] as f32, 250.0),
                                egui::Sense::hover(),
                            );
                            let painter = ui.painter_at(rect);
                            painter.image(
                                texture,
                                rect,
                                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                            painter.rect_stroke(
                                rect,
                                0.0,
                                egui::Stroke::new(1.0, egui::Color32::DARK_GRAY),
                            );
                            if *expanded_view == ExpandedView::LuminanceParade
                                && color_analysis.is_some_and(|analysis| analysis.luminance_is_pq)
                            {
                                luminance_guides(&painter, rect);
                            }
                        }
                    });
            }
        });
        let primitives = self
            .context
            .tessellate(output.shapes.clone(), output.pixels_per_point);

        // A fresh renderer has no font atlas and egui only sends deltas.
        if std::mem::take(&mut self.restore_font_texture) {
            let font_id = egui::TextureId::Managed(0);
            output.textures_delta.set.retain(|(id, _)| *id != font_id);
            output.textures_delta.set.push((
                font_id,
                egui::epaint::ImageDelta::full(
                    self.context.fonts(|fonts| fonts.image()),
                    egui::TextureOptions::LINEAR,
                ),
            ));
        }

        UiFrame {
            output,
            primitives,
            overlay: self.active_overlay,
            capture_pointer: self.context.wants_pointer_input(),
            capture_keyboard: self.context.wants_keyboard_input()
                || self.context.wants_pointer_input(),
        }
    }

    fn enum_checkbox(
        ui: &mut egui::Ui,
        current: &mut Option<ExpandedView>,
        value: ExpandedView,
        label: &str,
    ) {
        let mut checked = *current == Some(value);

        if ui.checkbox(&mut checked, label).changed() {
            *current = if checked { Some(value) } else { None };
        }
    }
}

fn luminance_legend(ui: &mut egui::Ui) {
    ui.label("Luminance (nits), PQ scale");
    // Keep in sync with shaders/luminance.frag.
    let colors = [
        [0, 0, 20],
        [25, 40, 100],
        [0, 100, 180],
        [0, 175, 190],
        [0, 180, 45],
        [235, 230, 0],
        [255, 65, 0],
        [255, 170, 120],
        [255, 240, 220],
    ];
    let (rect, _) = ui.allocate_exact_size(egui::vec2(320.0, 38.0), egui::Sense::hover());
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 12.0));
    let mut mesh = egui::Mesh::default();
    for (index, rgb) in colors.iter().enumerate() {
        let x = bar.left() + bar.width() * index as f32 / 8.0;
        let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        mesh.colored_vertex(egui::pos2(x, bar.top()), color);
        mesh.colored_vertex(egui::pos2(x, bar.bottom()), color);
        if index < 8 {
            let base = (index * 2) as u32;
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    for (nits, label) in [
        (0.0, "0"),
        (1.0, "1"),
        (10.0, "10"),
        (100.0, "100"),
        (1000.0, "1k"),
        (10000.0, "10k"),
    ] {
        let x = bar.left() + bar.width() * nits_to_pq(nits) as f32;
        ui.painter().line_segment(
            [
                egui::pos2(x, bar.bottom()),
                egui::pos2(x, bar.bottom() + 4.0),
            ],
            egui::Stroke::new(1.0, egui::Color32::GRAY),
        );
        let align = if nits == 0.0 {
            egui::Align2::LEFT_TOP
        } else if nits == 10000.0 {
            egui::Align2::RIGHT_TOP
        } else {
            egui::Align2::CENTER_TOP
        };
        ui.painter().text(
            egui::pos2(x, bar.bottom() + 5.0),
            align,
            label,
            egui::FontId::monospace(10.0),
            egui::Color32::LIGHT_GRAY,
        );
    }
    ui.label("HDR10 range: 0-10,000 nits");
}

fn update_texture(
    context: &egui::Context,
    texture: &mut Option<egui::TextureHandle>,
    name: &str,
    image: egui::ColorImage,
) {
    if let Some(texture) = texture {
        texture.set(image, egui::TextureOptions::NEAREST);
    } else {
        *texture = Some(context.load_texture(name, image, egui::TextureOptions::NEAREST));
    }
}

fn parade(ui: &mut egui::Ui, texture: egui::TextureId, show_nits: bool) {
    const SIZE: egui::Vec2 = egui::vec2(320.0, 100.0);

    let (rect, _) = ui.allocate_exact_size(SIZE, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.image(
        texture,
        rect,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, egui::Color32::DARK_GRAY));
    if show_nits {
        luminance_guides(&painter, rect);
    }
}

fn luminance_guides(painter: &egui::Painter, rect: egui::Rect) {
    for (nits, label) in [
        (100.0, "100 nits"),
        (300.0, "300 nits"),
        (1000.0, "1k nits"),
        (5000.0, "5k nits"),
    ] {
        let bin = (nits_to_pq(nits) * 1023.0).round() as f32;
        let y = rect.bottom() - rect.height() * ((bin + 0.5) / 1024.0);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(90)),
        );
        let position = egui::pos2(rect.left() + 4.0, y - 1.0);
        let font = egui::FontId::monospace(9.0);
        painter.text(
            position + egui::vec2(1.0, 1.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            font.clone(),
            egui::Color32::BLACK,
        );
        painter.text(
            position,
            egui::Align2::LEFT_BOTTOM,
            label,
            font,
            egui::Color32::LIGHT_GRAY,
        );
    }
}

fn histogram(ui: &mut egui::Ui, series: &[&[u32]], colors: &[egui::Color32], maximum: u32) {
    const SIZE: egui::Vec2 = egui::vec2(320.0, 100.0);

    let (rect, _) = ui.allocate_exact_size(SIZE, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(180));
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, egui::Color32::DARK_GRAY));

    if maximum == 0 {
        return;
    }

    for (values, color) in series.iter().zip(colors) {
        if values.len() < 2 {
            continue;
        }
        let points = values.iter().enumerate().map(|(index, count)| {
            let x = egui::lerp(rect.x_range(), index as f32 / (values.len() - 1) as f32);
            let y = rect.bottom() - rect.height() * (*count as f32 / maximum as f32);
            egui::pos2(x, y)
        });
        painter.add(egui::Shape::line(
            points.collect(),
            egui::Stroke::new(1.25, *color),
        ));
    }
}
