use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use csv::ReaderBuilder;
use serde::{Deserialize, Serialize};
use egui;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BooruTag {
    pub name: String,
    pub tag_type: i32,
    pub aliases: Vec<String>,
}

#[derive(Default)]
pub struct BooruTagManager {
    tags: HashMap<String, BooruTag>,
    tag_suggestions: Vec<String>,
    current_input: String,
    selected_suggestion: Option<usize>,
}

impl BooruTagManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_from_csv(&mut self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let mut rdr = ReaderBuilder::new()
            .has_headers(false)
            .from_reader(contents.as_bytes());

        for result in rdr.records() {
            let record = result?;
            if record.len() >= 4 {
                let name = record[0].to_string();
                let tag_type = record[1].parse::<i32>().unwrap_or(0);
                let aliases: Vec<String> = record[3]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect();

                self.tags.insert(name.clone(), BooruTag {
                    name,
                    tag_type,
                    aliases,
                });
            }
        }

        Ok(())
    }

    pub fn update_suggestions(&mut self, input: &str) {
        if input.is_empty() {
            self.tag_suggestions.clear();
            return;
        }

        let mut matches: Vec<_> = self.tags.values()
            .filter(|tag| {
                tag.name.contains(input) ||
                    tag.aliases.iter().any(|alias| alias.contains(input))
            })
            .map(|tag| tag.name.clone())
            .collect();

        matches.sort_by(|a, b| {
            let a_exact = a == input;
            let b_exact = b == input;
            let a_starts = a.starts_with(input);
            let b_starts = b.starts_with(input);

            if a_exact != b_exact {
                return b_exact.cmp(&a_exact);
            }
            if a_starts != b_starts {
                return b_starts.cmp(&a_starts);
            }
            a.cmp(b)
        });

        self.tag_suggestions = matches.into_iter().take(10).collect();
    }

    pub fn draw_tag_editor(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut selected_tag = None;

        let response = ui.text_edit_singleline(&mut self.current_input);

        if response.changed() {
            self.update_suggestions(&self.current_input);
        }

        if !self.tag_suggestions.is_empty() {
            egui::Window::new("Tag Suggestions")
                .fixed_size([300.0, 300.0])
                .collapsible(false)
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-5.0, 5.0))
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (idx, suggestion) in self.tag_suggestions.iter().enumerate() {
                            let is_selected = self.selected_suggestion == Some(idx);
                            let label = if let Some(tag_type) = self.get_tag_type(suggestion) {
                                let color = match tag_type {
                                    0 => egui::Color32::GRAY,    // General
                                    1 => egui::Color32::RED,     // Character
                                    3 => egui::Color32::GREEN,   // Copyright
                                    4 => egui::Color32::YELLOW,  // Meta
                                    _ => egui::Color32::WHITE,
                                };
                                ui.horizontal(|ui| {
                                    ui.colored_label(color, "●");
                                    ui.selectable_label(is_selected, suggestion)
                                }).inner
                            } else {
                                ui.selectable_label(is_selected, suggestion)
                            };

                            if label.clicked() {
                                selected_tag = Some(suggestion.clone());
                                self.current_input.clear();
                                self.tag_suggestions.clear();
                                break;
                            }
                        }
                    });
                });
        }

        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if !self.current_input.is_empty() {
                // If there are suggestions, use the first one
                if let Some(first_suggestion) = self.tag_suggestions.first() {
                    selected_tag = Some(first_suggestion.clone());
                }
                self.current_input.clear();
                self.tag_suggestions.clear();
            }
        }

        selected_tag
    }

    pub fn get_tag_type(&self, tag: &str) -> Option<i32> {
        self.tags.get(tag).map(|t| t.tag_type)
    }

    pub fn get_aliases(&self, tag: &str) -> Option<&Vec<String>> {
        self.tags.get(tag).map(|t| &t.aliases)
    }
}