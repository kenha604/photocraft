//! The start-up splash: artwork shown over the window while the app starts, where Photoshop shows
//! its splash screen. It holds for a moment, then fades; a click or key press closes it sooner.
//! Only the desktop app turns it on (`PhotocraftApp::splash`), so tests and the web runner start
//! straight into the UI.

use egui::{Color32, Context, Id, Rect, TextureHandle, TextureOptions, pos2};
use photocraft_codecs::{ChannelLayout, SampleType};

/// The artworks, one per start, in the order they take turns.
const ART: [&[u8]; 9] = [
    include_bytes!("../../../assets/splash/transport_08_clean.webp"),
    include_bytes!("../../../assets/splash/lizardfolk.jpg"),
    include_bytes!("../../../assets/splash/aarakocra.jpg"),
    include_bytes!("../../../assets/splash/spider-verse.jpg"),
    include_bytes!("../../../assets/splash/red-knight.jpg"),
    include_bytes!("../../../assets/splash/city-sunset.jpg"),
    include_bytes!("../../../assets/splash/y-wings.jpg"),
    include_bytes!("../../../assets/splash/snow-duel.jpg"),
    include_bytes!("../../../assets/splash/torii-fire.jpg"),
];

/// Seconds the artwork stays before it starts to fade, and the length of the fade.
const HOLD_S: f64 = 2.5;
const FADE_S: f64 = 0.4;
/// The artwork's share of the window, at most (never shown above its own size).
const MAX_SHARE: f32 = 0.7;

/// The splash's timing, in egui time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Splash {
    /// Which of [`ART`] this start shows.
    art: usize,
    started: Option<f64>,
    closing: Option<f64>,
}

/// Artwork `art`'s pixels (one transparent pixel if it couldn't be decoded, which a test rules
/// out).
fn decode(art: usize) -> egui::ColorImage {
    let rgba = ART.get(art).and_then(|bytes| photocraft_codecs::decode(bytes).ok()).map(|img| img.convert(ChannelLayout::Rgba, SampleType::U8));
    match rgba {
        Some(img) if img.data().len() == img.width() as usize * img.height() as usize * 4 => {
            egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.data())
        }
        _ => egui::ColorImage::filled([1, 1], Color32::TRANSPARENT),
    }
}

/// The artwork texture, uploaded on first use and kept in the context while the splash shows.
fn texture(ctx: &Context, art: usize) -> TextureHandle {
    let id = Id::new("photocraft-splash-art");
    if let Some(tex) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return tex;
    }
    let tex = ctx.load_texture("photocraft-splash-art", decode(art), TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

impl Splash {
    /// The splash of start number `n`: the artworks take turns from one start to the next.
    pub fn for_start(n: usize) -> Self {
        Self { art: n % ART.len(), ..Default::default() }
    }

    /// How opaque the splash is at `now` (1 while it holds, down to 0 through the fade), starting
    /// the fade after the hold or when `dismiss` asks.
    fn opacity(&mut self, now: f64, dismiss: bool) -> f32 {
        let started = *self.started.get_or_insert(now);
        if self.closing.is_none() && (dismiss || now - started >= HOLD_S) {
            self.closing = Some(now);
        }
        self.closing.map_or(1.0, |t| (1.0 - (now - t) / FADE_S).clamp(0.0, 1.0) as f32)
    }

    /// Paint the splash over everything this frame. `false` once it has faded out: drop it.
    pub fn show(&mut self, ctx: &Context) -> bool {
        let (now, dismiss) = ctx.input(|i| (i.time, i.pointer.any_pressed() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))));
        let opacity = self.opacity(now, dismiss);
        if opacity <= 0.0 {
            ctx.data_mut(|d| d.remove::<TextureHandle>(Id::new("photocraft-splash-art")));
            return false;
        }
        let screen = ctx.content_rect();
        let tex = texture(ctx, self.art);
        let size = tex.size_vec2();
        let scale = (screen.width() * MAX_SHARE / size.x).min(screen.height() * MAX_SHARE / size.y).min(1.0);
        let art = Rect::from_center_size(screen.center(), size * scale);
        let area = egui::Area::new(Id::new("photocraft-splash")).order(egui::Order::Foreground).fixed_pos(screen.min).interactable(true).show(ctx, |ui| {
            // Clicks close the splash; they don't reach the UI under it.
            ui.allocate_rect(screen, egui::Sense::click());
            let painter = ui.painter().with_clip_rect(screen);
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha((170.0 * opacity) as u8));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(tex.id(), art, uv, Color32::WHITE.gamma_multiply(opacity));
        });
        ctx.move_to_top(area.response.layer_id);
        ctx.request_repaint();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_artwork_decodes() {
        for art in 0..ART.len() {
            let img = decode(art);
            assert!(img.size[0] >= 1000 && img.size[1] >= 400, "artwork {art}: {:?}", img.size);
            assert!(img.pixels.iter().any(|p| p.r() > 200), "artwork {art} isn't blank");
        }
    }

    #[test]
    fn the_artworks_take_turns_from_start_to_start() {
        let shown: Vec<usize> = (0..ART.len() + 2).map(|n| Splash::for_start(n).art).collect();
        assert_eq!(shown, [0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 1]);
        assert_eq!(Splash::for_start(usize::MAX).art, usize::MAX % ART.len());
    }

    #[test]
    fn it_holds_then_fades_and_a_click_closes_it_sooner() {
        let mut s = Splash::default();
        assert_eq!(s.opacity(10.0, false), 1.0);
        assert_eq!(s.opacity(10.0 + HOLD_S - 0.1, false), 1.0);
        assert_eq!(s.opacity(10.0 + HOLD_S, false), 1.0, "the fade starts here");
        let half = s.opacity(10.0 + HOLD_S + FADE_S / 2.0, false);
        assert!((half - 0.5).abs() < 1e-3, "{half}");
        assert_eq!(s.opacity(10.0 + HOLD_S + FADE_S, false), 0.0);

        let mut s = Splash::default();
        assert_eq!(s.opacity(0.0, false), 1.0);
        assert_eq!(s.opacity(0.5, true), 1.0);
        assert_eq!(s.opacity(0.5 + FADE_S, false), 0.0);
    }

    #[test]
    fn it_paints_over_the_window_until_it_has_faded() {
        let ctx = Context::default();
        let mut s = Splash::default();
        let mut frame = |time: f64| {
            let mut alive = false;
            let mut out = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| alive = s.show(ui.ctx()));
            out.textures_delta.clear();
            alive
        };
        assert!(frame(0.0));
        assert!(frame(HOLD_S + FADE_S / 2.0));
        assert!(!frame(HOLD_S + FADE_S * 2.0));
    }
}
