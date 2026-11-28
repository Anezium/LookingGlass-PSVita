//! The archive camera's layers and shutter timing, independent of save actions.
use super::*;
use macroquad::miniquad::{
    BlendFactor, BlendState, BlendValue, Equation, PipelineParams, ShaderSource,
};

const VERTEX: &str = r#"#version 100
attribute vec3 position; attribute vec2 texcoord; attribute vec4 color0;
uniform mat4 Model; uniform mat4 Projection;
varying lowp vec4 color; varying mediump vec2 uv;
void main(){gl_Position=Projection*Model*vec4(position,1.0);uv=texcoord;color=color0/255.0;}
"#;
const FRAGMENT: &str = r#"#version 100
precision mediump float;
varying lowp vec4 color; varying mediump vec2 uv;
uniform sampler2D Texture;
void main(){gl_FragColor=texture2D(Texture,uv)*color;}
"#;

#[derive(Default)]
struct Shutter {
    from: usize,
    to: usize,
    start: f64,
    opening: bool,
}
impl Shutter {
    fn sample(&self, now: f64) -> (usize, f32) {
        let t = (now - self.start).max(0.) as f32;
        if self.opening {
            (self.to, (t / 0.18).min(1.))
        } else {
            // loadsave.urc: close in 180 ms, select at 350 ms, reopen at 500 ms.
            let open = if t < 0.18 {
                1. - t / 0.18
            } else {
                ((t - 0.5) / 0.18).clamp(0., 1.)
            };
            (if t < 0.35 { self.from } else { self.to }, open)
        }
    }
    fn select(&mut self, index: usize, now: f64) {
        self.from = self.sample(now).0;
        self.to = index;
        self.start = now;
        self.opening = false;
    }
}

pub(super) struct Film {
    material: Material,
    grain: Vec<Texture2D>,
    shutter: Shutter,
    sounds: [std::sync::Arc<[u8]>; 3],
    playing: Vec<crate::audio::sink::Sink>,
}
impl Film {
    pub fn load(assets: &mut Assets) -> Result<Self> {
        let material = crate::frame_profile::load_material(
            ShaderSource::Glsl {
                vertex: VERTEX,
                fragment: FRAGMENT,
            },
            MaterialParams {
                pipeline_params: PipelineParams {
                    // scripts/ui.shader, ui/loadgame/sepia1. This is a film
                    // projection over the photo, not an opaque brown rectangle.
                    color_blend: Some(BlendState::new(
                        Equation::Add,
                        BlendFactor::Value(BlendValue::DestinationColor),
                        BlendFactor::Value(BlendValue::SourceAlpha),
                    )),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("Save camera material: {e:?}"))?;
        let grain = (2..=6)
            .map(|i| load_texture(assets, &format!("ui/load/sepia1_{i}.tga"), false))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            material,
            grain,
            shutter: Shutter::default(),
            playing: Vec::new(),
            sounds: [
                assets.read("sound/ui/projector_long.wav")?.into(),
                assets.read("sound/ui/hover_3.wav")?.into(),
                assets.read("sound/ui/click_4.wav")?.into(),
            ],
        })
    }
    pub fn open(&mut self, index: usize, now: f64) {
        self.shutter = Shutter {
            from: index,
            to: index,
            start: now,
            opening: true,
        };
    }
    pub fn select(&mut self, index: usize, now: f64) {
        self.shutter.select(index, now);
    }
    pub fn sound(&mut self, audio: &audio::Audio, index: usize) {
        match audio.menu_sound(self.sounds[index].clone()) {
            Ok(Some(sink)) => self.playing.push(sink),
            Ok(None) => (),
            Err(e) => eprintln!("Menu sound unavailable: {e:#}"),
        }
        // Rapid slot changes never leave an unbounded queue of projector sounds.
        if self.playing.len() > 4 {
            self.playing.remove(0).stop();
        }
    }
    pub fn update_audio(&mut self, settings: audio::Settings, focused: bool) {
        self.playing.retain(|s| !s.empty());
        for sink in &self.playing {
            sink.set_volume(if settings.muted { 0. } else { settings.effects });
            if focused {
                sink.play();
            } else {
                sink.pause();
            }
        }
    }
    pub fn stop(&mut self) {
        for sink in self.playing.drain(..) {
            sink.stop();
        }
    }
    fn project(&self, canvas: Canvas, rect: Rect, now: f64) {
        let tick = (now.max(0.) * 10.) as usize;
        // A stable bounded noise sample gives the original 10 Hz alpha flicker.
        let noise =
            ((tick as u32).wrapping_mul(1664525).wrapping_add(1013904223) >> 16) as f32 / 65535.;
        let r = canvas.rect(rect);
        gl_use_material(&self.material);
        draw_texture_ex(
            &self.grain[tick % self.grain.len()],
            r.x,
            r.y,
            Color::new(1., 1., 1., 0.6 + 0.4 * noise),
            DrawTextureParams {
                dest_size: Some(r.size()),
                ..Default::default()
            },
        );
        gl_use_default_material();
    }
}

/// The page Prev/Next lead to. Pages stop at both ends (no wrap); without `save_pages` there is
/// one page and the buttons do nothing.
pub(super) fn turn_page(page: usize, delta: i32, pages: bool) -> usize {
    if !pages {
        return 0;
    }
    (page as i64 + i64::from(delta)).clamp(0, Slot::PAGES as i64 - 1) as usize
}

/// Whether Delete may open its confirmation for `slot`, or the message the screen shows instead.
pub(super) fn delete_request(slot: Slot, exists: bool) -> std::result::Result<(), &'static str> {
    if !slot.deletable() {
        Err("The automatic save cannot be deleted.")
    } else if !exists {
        Err("This slot has no save to delete.")
    } else {
        Ok(())
    }
}

/// UTC is explicit so saves remain unambiguous across time-zone changes.
pub(super) fn timestamp(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = (seconds / 86400).min(2_932_896) as i64;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{day} {} {year}  {:02}:{:02} UTC",
        names[month as usize - 1],
        seconds / 3600 % 24,
        seconds / 60 % 60
    )
}

impl Menu {
    fn save_widget(&self, name: &str) -> &Widget {
        self.layouts["loadsave"]
            .iter()
            .find(|w| w.value("name") == name)
            .expect("validated archive save widget")
    }
    fn shot_rect(&self, index: usize) -> Rect {
        self.save_widget(&format!("LoadSaveShot{}", index + 1)).rect
    }
    pub(super) fn slot_rect(&self, index: usize) -> Rect {
        let shot = self.shot_rect(index);
        let button = self.save_widget(&format!("LoadButton{}", index + 1)).rect;
        Rect::new(shot.x, shot.y, shot.w, button.bottom() - shot.y)
    }
    fn shutter_art(&self, canvas: Canvas, name: &str, offset: f32, clip: Rect) {
        let w = self.save_widget(name);
        let r = Rect::new(w.rect.x + offset, w.rect.y, w.rect.w, w.rect.h);
        let x = r.x.max(clip.x);
        let y = r.y.max(clip.y);
        let width = r.right().min(clip.right()) - x;
        let height = r.bottom().min(clip.bottom()) - y;
        if width <= 0. || height <= 0. {
            return;
        }
        if let Some(t) = self.textures.get(&w.art().to_ascii_lowercase()) {
            let dest = canvas.rect(Rect::new(x, y, width, height));
            draw_texture_ex(
                t,
                dest.x,
                dest.y,
                WHITE,
                DrawTextureParams {
                    dest_size: Some(dest.size()),
                    source: Some(Rect::new(
                        (x - r.x) / r.w * t.width(),
                        (y - r.y) / r.h * t.height(),
                        width / r.w * t.width(),
                        height / r.h * t.height(),
                    )),
                    ..Default::default()
                },
            );
        }
    }
    pub(super) fn draw_save_underlay(&self, canvas: Canvas, now: f64) {
        for index in 0..6 {
            self.save_preview(canvas, index, self.shot_rect(index));
        }
        let (shown, open) = self.save_film.shutter.sample(now);
        let rect = self.save_widget("BigSaveShot").rect;
        self.save_preview(canvas, shown, rect);
        self.save_film.project(canvas, rect, now);
        self.shutter_art(canvas, "bigcover_left", -190. * open, rect);
        self.shutter_art(canvas, "bigcover_right", 190. * open, rect);
    }
    pub(super) fn draw_save_details(&self, canvas: Canvas, page: Page) {
        for (index, slot) in (0..6).map(|i| (i, Slot::on_page(self.save_page, i))) {
            let b = self.save_widget(&format!("LoadButton{}", index + 1));
            let lit =
                index == self.save_selected || page == Page::LoadSave && index == self.selected;
            self.art(
                canvas,
                if lit { b.value("hovershader") } else { b.art() },
                b.rect,
                if lit {
                    0.9 + (get_time() as f32 * std::f32::consts::PI).sin() * 0.1
                } else {
                    1.
                },
            );
            let x = if index % 2 == 0 {
                b.rect.right() + 3.
            } else {
                self.shot_rect(index).x + 3.
            };
            self.ui.font.draw(
                canvas,
                &if slot == Slot::Auto {
                    "Autosave".into()
                } else {
                    slot.title()
                },
                Rect::new(x, b.rect.y + 3., 80., 22.),
                16.,
                crate::ui::PAPER_TEXT,
            );
        }
        // Without their keys the pagination/delete hardware stays visible, unlit
        // and without actions; with a key on, the button loop draws it.
        for (name, wired) in [
            ("PrevButton", self.save_keys.pages),
            ("NextButton", self.save_keys.pages),
            ("DeleteButton", self.save_keys.delete),
        ] {
            if !wired {
                let w = self.save_widget(name);
                self.art(canvas, w.art(), w.rect, 0.65);
            }
        }
        let page_num = self.save_widget("PageNum").rect;
        self.ui.font.draw(
            canvas,
            &(self.save_page + 1).to_string(),
            page_num,
            18.,
            crate::ui::PAPER_TEXT,
        );
        let shown = self.save_film.shutter.sample(get_time()).0;
        let title = self.save_widget("MapName").rect;
        self.ui
            .font
            .draw(canvas, &self.save_labels[shown], title, 23., crate::ui::INK);
        let status = if page != Page::LoadSave {
            &self.save_dates[shown]
        } else if self.save_keys == MenuKeys::default() {
            match self.selected {
                6 => "Load Game",
                7 if self.save_selected == 5 => "Automatic save",
                7 => "Save Game",
                _ => &self.save_dates[shown],
            }
        } else {
            // The keys add buttons: name the selected button by its action (urc mouseEnterCmd).
            match self.buttons(Page::LoadSave).get(self.selected).map(|b| b.click) {
                Some(Click::LoadSelection) => "Load Game",
                Some(Click::SaveSelection) if self.selected_slot() == Slot::Auto => "Automatic save",
                Some(Click::SaveSelection) => "Save Game",
                Some(Click::DeleteSelection) => "Delete Game",
                _ => &self.save_dates[shown],
            }
        };
        self.ui.font.draw(
            canvas,
            status,
            self.save_widget("DateTime").rect,
            18.,
            crate::ui::INK,
        );
    }
}

impl Menu {
    /// loadsave.urc `delete_group`: the DeleteConfirm art ("Delete? yes no") fades in over the
    /// big preview in 0.5 s; DeleteYES and DeleteNO are invisible hit boxes over its words. The
    /// selected word gets a thin pulsing underline so the D-pad choice is visible.
    pub(super) fn draw_delete_confirm(&self, canvas: Canvas, now: f64) {
        let fade = ((now - self.delete_opened) / 0.5).clamp(0., 1.) as f32;
        let w = self.save_widget("DeleteConfirm");
        self.art(canvas, w.art(), w.rect, fade);
        let choice = self.save_widget(if self.selected == 0 { "DeleteYES" } else { "DeleteNO" }).rect;
        let r = canvas.rect(Rect::new(choice.x + 4., choice.bottom() - 2., choice.w - 8., 2.));
        let pulse = 0.9 + (now as f32 * std::f32::consts::PI).sin() * 0.1;
        draw_rectangle(r.x, r.y, r.w, r.h, Color::new(1., 1., 1., pulse * fade));
    }
}

/// Host proof only: `LOOKING_GLASS_MENU_SHOTS=<dir>` prints every menu state change and saves one
/// PNG per page state once its shutters have settled.
#[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
#[derive(Default)]
pub(super) struct Probe {
    state: String,
    since: f64,
    shot: bool,
    shots: u32,
    line: String,
}
#[cfg(all(feature = "sim_maps_probe", not(target_os = "vita")))]
impl Menu {
    pub(super) fn probe_menu(&mut self, page: Page) {
        let Some(dir) = std::env::var_os("LOOKING_GLASS_MENU_SHOTS") else {
            return;
        };
        let now = get_time();
        let state = format!("{page:?}-page{}", self.save_page + 1);
        let line = format!(
            "MENU_STATE state={state} selected={} save_selected={} message={:?}",
            self.selected, self.save_selected, self.message
        );
        if line != self.probe.line {
            println!("{line}");
            self.probe.line = line;
        }
        if state != self.probe.state {
            self.probe.state = state;
            self.probe.since = now;
            self.probe.shot = false;
        }
        if !self.probe.shot && now - self.probe.since >= 0.8 {
            self.probe.shot = true;
            self.probe.shots += 1;
            let name: String = self
                .probe
                .state
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
                .collect();
            let path = std::path::Path::new(&dir).join(format!("{:02}-{name}.png", self.probe.shots));
            match crate::viewer::save_capture(&path) {
                Ok(()) => println!("MENU_SHOT {}", path.display()),
                Err(e) => println!("MENU_SHOT failed {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pages_stop_at_both_ends_and_need_the_key() {
        assert_eq!(turn_page(0, -1, true), 0);
        assert_eq!(turn_page(0, 1, true), 1);
        let last = Slot::PAGES - 1;
        assert_eq!(turn_page(last, 1, true), last);
        assert_eq!(turn_page(last, -1, true), last - 1);
        let mut page = 0;
        for _ in 0..20 {
            page = turn_page(page, 1, true);
        }
        assert_eq!(page, last);
        for page in 0..Slot::PAGES {
            assert_eq!(turn_page(page, 1, false), 0);
            assert_eq!(turn_page(page, -1, false), 0);
        }
    }
    #[test]
    fn delete_needs_an_existing_deletable_slot() {
        assert_eq!(delete_request(Slot::One, true), Ok(()));
        assert_eq!(delete_request(Slot::Quick, true), Ok(()));
        assert_eq!(delete_request(Slot::Extra(12), true), Ok(()));
        assert!(delete_request(Slot::Auto, true).is_err());
        assert!(delete_request(Slot::Two, false).is_err());
    }
    #[test]
    fn cancelling_the_confirmation_keeps_every_save() {
        // DeleteNO and Escape are Back to the save screen: no store call at all.
        assert_eq!(Page::ConfirmDelete(Slot::Two).parent(), Some(Page::LoadSave));
        assert_eq!(Page::ConfirmDelete(Slot::Two).layout(), "loadsave");
        let scratch = crate::save::format12::Scratch::new("delete-cancel");
        let store = scratch.store();
        let game = crate::save::tests::jlair2_game();
        for slot in [Slot::One, Slot::Two, Slot::Extra(5)] {
            store.write(slot, &game).unwrap();
            store.write(slot, &game).unwrap();
        }
        let snapshot = || {
            let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&scratch.0)
                .unwrap()
                .map(|e| e.unwrap().path())
                .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(p).unwrap()))
                .collect();
            files.sort();
            files
        };
        let before = snapshot();
        assert_eq!(before.len(), 6);
        assert_eq!(delete_request(Slot::Two, store.exists(Slot::Two)), Ok(()));
        // The menu opens ConfirmDelete(Two); No returns to LoadSave without touching the store.
        assert_eq!(snapshot(), before);
    }
    #[test]
    fn selection_changes_behind_closed_shutters_and_rapid_clicks_settle() {
        let mut s = Shutter {
            from: 0,
            to: 0,
            start: 0.,
            opening: true,
        };
        s.select(1, 1.);
        assert_eq!(s.sample(1.), (0, 1.));
        assert_eq!(s.sample(1.2), (0, 0.));
        assert_eq!(s.sample(1.4), (1, 0.));
        assert_eq!(s.sample(1.8), (1, 1.));
        s.select(2, 1.45);
        assert_eq!(s.sample(1.45).0, 1);
        assert_eq!(s.sample(2.2), (2, 1.));
    }
    #[test]
    fn save_dates_handle_epoch_and_leap_day() {
        assert_eq!(timestamp(0), "1 Jan 1970  00:00 UTC");
        assert_eq!(timestamp(951827696000), "29 Feb 2000  12:34 UTC");
        assert_eq!(timestamp(1790596800000), "28 Sep 2026  12:00 UTC");
    }
}
