use super::*;
use crate::{npc::Puppet, weapons::Prop};
pub(super) struct Art {
    boss: Puppet,
    alice: Puppet,
    ant: Puppet,
    shots: [Prop; 2],
    input: Option<DrawInput>,
}
impl Art {
    pub fn load(a: &mut Assets) -> Result<Self> {
        let specs = crate::texture::read_materials(a)?;
        let mut boss = Puppet::load(a, "c_centipede", data::BOSS, &specs)?;
        boss.mouth_angle(45.);
        boss.surface_visible("material4", false);
        Ok(Self {
            boss,
            alice: Puppet::load(a, "alice", data::ALICE, &specs)?,
            input: None,
            ant: Puppet::load(a, "c_armyant", crate::ant::REGULAR, &specs)?,
            shots: [
                Prop::load_animation(a, "prj_bullet", "idle", &specs)?,
                Prop::load_animation(a, "prj_grenade", "idle", &specs)?,
            ],
        })
    }
}
struct BossDraw {
    clip: &'static str,
    time: f32,
    looping: bool,
    pose: Transform,
    alice: Transform,
    alice_time: f32,
}
struct AntDraw {
    ant: crate::ant::Ant,
    moving: bool,
    scale: f32,
}
struct DrawInput {
    mouth: Option<f32>,
    boss: Option<BossDraw>,
    ants: Vec<AntDraw>,
}

impl DrawInput {
    fn capture(o: &Flora, mouth: Option<f32>) -> Self {
        let boss = o.reveal().then(|| {
            let (clip, time, looping) = o.boss_clip();
            BossDraw {
                clip, time, looping, pose: o.data.points["cent_posx1"],
                alice: o.saved.alice, alice_time: o.saved.time as f32,
            }
        });
        let ants = o.saved.ants.iter().filter(|s| s.ant.enabled).map(|s| {
            let ant = &s.ant;
            AntDraw {
                ant: ant.clone(),
                moving: o.saved.phase == Phase::Rush && o.data.runners.contains_key(&ant.id)
                    && ant.health > 0.,
                scale: ant.visual_scale(&o.data),
            }
        }).collect();
        Self { mouth, boss, ants }
    }

    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.ants.len() * std::mem::size_of::<AntDraw>()
            + self.ants.iter().map(|a| std::mem::size_of_val(a.ant.shots.as_slice())
                + std::mem::size_of_val(a.ant.blasts.as_slice())).sum::<usize>()
    }
}

impl LevelArt for Art {
    fn extract_snapshot(&mut self, level: &dyn LevelController, story: &Story) -> (usize, bool) {
        let o = level.downcast_ref::<Flora>().expect("Flora controller");
        self.input = (o.saved.ants.len() <= 24
            && o.saved.ants.iter().all(|s| s.ant.shots.len() <= 32 && s.ant.blasts.len() <= 32))
            .then(|| DrawInput::capture(o, Some(story.mouth(&["centipede_actor1"]))));
        (self.snapshot_bytes(), !self.snapshot_ready())
    }

    fn snapshot_ready(&self) -> bool { self.input.is_some() }
    fn snapshot_bytes(&self) -> usize { self.input.as_ref().map_or(0, DrawInput::bytes) }
    fn clear_snapshot(&mut self) { self.input = None; }

    fn draw_snapshot(&mut self, atm: &crate::environment::Atmosphere, camera: Vec3, bright: bool) -> bool {
        let Some(input) = self.input.take() else { return false; };
        self.draw_input(&input, atm, camera, bright);
        self.input = Some(input);
        true
    }

    fn effects_snapshot(&mut self, camera: Vec3, atm: &crate::environment::Atmosphere) -> bool {
        let Some(input) = self.input.take() else { return false; };
        self.draw_effects(&input, camera, atm);
        self.input = Some(input);
        true
    }

    fn story_pose(&mut self, s: &Story) {
        self.boss.mouth(s.mouth(&["centipede_actor1"]));
    }

    fn draw(&mut self, l: &dyn LevelController, atm: &crate::environment::Atmosphere, camera: Vec3, bright: bool) {
        self.draw_input(&DrawInput::capture(l.downcast_ref::<Flora>().unwrap(), None), atm, camera, bright);
    }

    fn effects(&mut self, l: &dyn LevelController, camera: Vec3, atm: &crate::environment::Atmosphere) {
        self.draw_effects(&DrawInput::capture(l.downcast_ref::<Flora>().unwrap(), None), camera, atm);
    }

    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

impl Art {
    fn draw_input(&mut self, input: &DrawInput, atm: &crate::environment::Atmosphere, camera: Vec3, bright: bool) {
        if let Some(mouth) = input.mouth {
            self.boss.mouth(mouth);
        }
        for a in [&mut self.boss, &mut self.alice, &mut self.ant] {
            a.atmosphere(atm, camera);
        }
        if let Some(boss) = &input.boss {
            self.boss.draw(boss.clip, boss.time, boss.looping, boss.pose, 1., bright);
            self.alice.draw("idle_stand", boss.alice_time, true, boss.alice, 1., bright);
        }
        for AntDraw { ant, moving, scale } in &input.ants {
            let electric_consumer = self.ant.electric_consumer(ant.electric);
            self.ant.draw(
                if *moving { "walk_fast" } else { ant.clip() },
                ant.time, *moving || ant.loops(), ant_pose(ant), *scale, bright,
            );
            drop(electric_consumer);
            self.ant.draw_electric(ant.electric);
            for shot in &ant.shots {
                crate::ant::draw_shot(&mut self.shots, shot, bright);
            }
        }
    }

    fn draw_effects(&mut self, input: &DrawInput, camera: Vec3, atm: &crate::environment::Atmosphere) {
        if let Some(boss) = &input.boss {
            self.boss.draw_effects(boss.clip, boss.time, boss.looping, boss.pose, 1., camera, atm);
        }
        for AntDraw { ant, scale, .. } in &input.ants {
            self.ant.draw_effects(ant.clip(), ant.time, ant.loops(), ant_pose(ant), *scale, camera, atm);
            crate::ant::draw_blasts(ant);
        }
    }
}

fn ant_pose(ant: &crate::ant::Ant) -> Transform {
    Transform {
        translation: ant.feet,
        rotation: Quat::from_rotation_z(ant.yaw),
    }
}
