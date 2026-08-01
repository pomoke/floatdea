mod data;

use std::collections::{HashMap, HashSet};

use eframe::{App, egui};

use crate::data::Snippet;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640., 480.]),
        ..Default::default()
    };

    eframe::run_native(
        "floatdea",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::<HomePage>::default())
        }),
    )
}

#[derive(Clone, Debug)]
struct HomePage {
    items: Vec<Snippet>,
    sub_windows: HashSet<usize>,
    close_windows: Vec<usize>,
}

impl Default for HomePage {
    fn default() -> Self {
        HomePage {
            items: vec![
                Snippet {
                    title: "hello".to_owned(),
                    content: "hello, world!".to_owned(),
                },
                Snippet {
                    title: "floatdea".to_owned(),
                    content: "Welcome to FloatDea!".to_owned(),
                },
            ],
            sub_windows: HashSet::new(),
            close_windows: vec![],
        }
    }
}

impl App for HomePage {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(16))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                for (id, item) in self.items.iter().enumerate() {
                    let btn = ui.button(&item.title);
                    btn.context_menu(|ui| if ui.button("delete").clicked() {});
                    if btn.clicked() {
                        self.sub_windows.insert(id);
                    }
                }
            });

        for id in &self.sub_windows {
            let title = self.items[*id].title.as_str();
            let content = self.items[*id].content.as_str();
            let close = ui.show_viewport_immediate(
                egui::ViewportId::from_hash_of(title),
                egui::ViewportBuilder::default(),
                |child_ui, _viewport_class| {
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::new()
                                .inner_margin(egui::Margin::same(16))
                                .fill(child_ui.visuals().panel_fill),
                        )
                        .show(child_ui, |ui| {
                            ui.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                                "{} - FloatDea",
                                title
                            )));

                            egui::Frame::new()
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical()
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(content).size(15.0),
                                                )
                                                .selectable(true)
                                                .wrap(),
                                            );
                                        });
                                });
                        });

                    if child_ui.input(|input| input.viewport().close_requested()) {
                        return true;
                    }
                    false
                },
            );
            if close {
                self.close_windows.push(*id);
            }
        }
        for id in &self.close_windows {
            self.sub_windows.remove(id);
        }
        self.close_windows.clear();
    }
}
