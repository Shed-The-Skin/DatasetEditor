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
    is_focused: bool,
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

    pub fn get_tag_type(&self, tag: &str) -> Option<i32> {
        self.tags.get(tag).map(|t| t.tag_type)
    }

    pub fn get_aliases(&self, tag: &str) -> Option<&Vec<String>> {
        self.tags.get(tag).map(|t| &t.aliases)
    }
    pub fn update_suggestions(&mut self, input: &str) {
        if input.is_empty() {
            self.tag_suggestions.clear();
            return;
        }

        // Convert input spaces to underscores for matching
        let search_input = input.replace(' ', "_");

        let mut matches: Vec<_> = self.tags.values()
            .filter(|tag| {
                tag.name.contains(&search_input) ||
                    tag.aliases.iter().any(|alias| alias.contains(&search_input))
            })
            .map(|tag| tag.name.clone())
            .collect();

        matches.sort_by(|a, b| {
            let a_exact = a == &search_input;
            let b_exact = b == &search_input;
            let a_starts = a.starts_with(&search_input);
            let b_starts = b.starts_with(&search_input);

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

        // Render the text input field
        let response = ui.text_edit_singleline(&mut self.current_input);
        self.is_focused = response.has_focus();

        // Update suggestions on input change
        if response.changed() {
            let current_input = self.current_input.clone(); // Clone the input to avoid conflicts
            self.update_suggestions(&current_input);
            println!("Input changed: {}", current_input);
        }


        // Handle Enter key to add tag
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if !self.current_input.trim().is_empty() {
                println!("Enter pressed with input: {}", self.current_input);
                selected_tag = Some(self.current_input.clone());
                self.current_input.clear();
                self.tag_suggestions.clear();
                return selected_tag;
            }
        }

        // Display suggestions in a pop-up
        if !self.tag_suggestions.is_empty() {
            // Clone the suggestions to avoid borrow conflicts
            let suggestions = self.tag_suggestions.clone();

            egui::Window::new("Tag Suggestions")
                .fixed_size([300.0, 300.0])
                .collapsible(false)
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-5.0, 5.0))
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for suggestion in suggestions {
                            ui.horizontal(|ui| {
                                if ui.button(&suggestion).clicked() {
                                    println!("Pop-up tag clicked: {}", suggestion);

                                    // Update input and simulate adding the tag
                                    self.current_input = suggestion.clone();
                                    selected_tag = Some(self.current_input.clone());
                                    self.current_input.clear();
                                    self.tag_suggestions.clear();

                                    println!("Selected tag for addition: {:?}", selected_tag);
                                }
                            });
                        }
                    });
                });
        }

        selected_tag
    }

}