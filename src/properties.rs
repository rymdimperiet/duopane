//! Properties (Alt+Enter): what quick view tells about a file or folder, in a
//! dialog, with its permissions to change. X toggles "can be run as a program",
//! Enter applies, Esc closes without changing anything.

use crate::theme;
use eframe::egui::{self, Key, Modifiers};
use std::path::PathBuf;

pub struct Properties {
    pub path: PathBuf,
    pub name: String,
    rows: Vec<(String, String)>,
    /// Permission bits as found, and as ticked in the dialog (None: can't be changed here).
    mode: Option<u32>,
    edit: u32,
}

pub enum Outcome {
    Stay,
    Close,
    /// Close and set these permission bits.
    Apply(u32),
}

impl Properties {
    pub fn new(path: PathBuf) -> Properties {
        let info = crate::quickview::general(&path);
        let mut rows: Vec<(String, String)> = info.sections.into_iter().flat_map(|(_, r)| r).filter(|(k, _)| k != "Permissions").collect();
        let name = info.name;
        if !info.is_dir {
            if let Some(app) = crate::open_with::default_app(&name, false) {
                rows.insert(1, ("Opens with".into(), app.name.clone()));
            }
        }
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&path).ok().map(|m| m.permissions().mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let mode = None;
        Properties { path, name, rows, mode, edit: mode.unwrap_or(0) }
    }

    /// Whether the ticked permissions differ from the file's.
    pub fn changed(&self) -> bool {
        self.mode.is_some_and(|m| m != self.edit)
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Outcome {
        let (enter, esc, x) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::NONE, Key::X),
            )
        });
        if x && self.mode.is_some() {
            self.edit = toggle_runnable(self.edit);
        }
        let mut out = if esc { Outcome::Close } else { Outcome::Stay };
        let mut ok = enter;
        let th = theme::c();
        crate::dialogs::dialog(&format!("Properties of {}", self.name)).show(ctx, |ui| {
            ui.set_width(520.0);
            egui::Grid::new("props").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for (k, v) in &self.rows {
                    ui.label(egui::RichText::new(k).color(th.dim));
                    ui.label(v);
                    ui.end_row();
                }
            });
            if self.mode.is_some() {
                ui.add_space(10.0);
                ui.separator();
                let mut run = self.edit & 0o111 != 0;
                if ui.checkbox(&mut run, "Can be run as a program  (X)").changed() {
                    self.edit = toggle_runnable(self.edit);
                }
                ui.add_space(4.0);
                egui::Grid::new("perm_bits").num_columns(4).spacing([18.0, 4.0]).show(ui, |ui| {
                    ui.label("");
                    for h in ["Read", "Write", "Run / open"] {
                        ui.label(egui::RichText::new(h).color(th.dim).small());
                    }
                    ui.end_row();
                    for (who, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
                        ui.label(egui::RichText::new(who).color(th.dim));
                        for bit in [4u32, 2, 1] {
                            let mask = bit << shift;
                            let mut on = self.edit & mask != 0;
                            if ui.checkbox(&mut on, "").changed() {
                                self.edit ^= mask;
                            }
                        }
                        ui.end_row();
                    }
                });
                ui.label(egui::RichText::new(format!("{:o}", self.edit)).color(th.dim).small());
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if self.changed() { "Apply" } else { "Close" };
                if ui.button(label).clicked() {
                    ok = true;
                }
                if self.changed() && ui.button("Cancel").clicked() {
                    out = Outcome::Close;
                }
                let hint = if self.changed() { "Enter applies, Esc cancels" } else { "Enter or Esc closes" };
                ui.label(egui::RichText::new(hint).color(th.dim).small());
            });
        });
        if ok && !matches!(out, Outcome::Close) {
            out = if self.changed() { Outcome::Apply(self.edit) } else { Outcome::Close };
        }
        out
    }
}

/// "Can be run": on, everyone who can read it may run it; off, nobody can.
fn toggle_runnable(mode: u32) -> u32 {
    if mode & 0o111 != 0 {
        mode & !0o111
    } else {
        mode | ((mode & 0o444) >> 2) | 0o100
    }
}

/// Sets permission bits (on Unix; elsewhere there's nothing to set).
pub fn apply(path: &std::path::Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runnable_follows_who_can_read() {
        assert_eq!(toggle_runnable(0o644), 0o755);
        assert_eq!(toggle_runnable(0o600), 0o700);
        assert_eq!(toggle_runnable(0o755), 0o644);
        assert_eq!(toggle_runnable(0o4755), 0o4644);
    }

    #[cfg(unix)]
    #[test]
    fn dialog_reads_and_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("duopane-props-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("run.sh");
        std::fs::write(&f, "echo hi").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut p = Properties::new(f.clone());
        assert!(p.rows.iter().any(|(k, _)| k == "Size"));
        assert!(!p.changed());
        p.edit = toggle_runnable(p.edit);
        assert!(p.changed());
        apply(&f, p.edit).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o7777, 0o755);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
