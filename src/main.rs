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
        for (id, item) in self.items.iter().enumerate() {
            if ui.button(&item.title).clicked() {
                self.sub_windows.insert(id);
            }
        }

        for id in &self.sub_windows {
            let title = self.items[*id].title.as_str();
            let content = self.items[*id].content.as_str();
            let close = ui.show_viewport_immediate(
                egui::ViewportId::from_hash_of(title),
                egui::ViewportBuilder::default(),
                |child_ui, _viewport_class| {
                    child_ui.heading(title);
                    child_ui.label(content);

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
