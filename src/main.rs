use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use eframe::egui;
use egui::Context;
use walkdir::WalkDir;

#[derive(Default, Clone)] // Added Clone
struct ImageData {
    path: PathBuf,
    tags: Vec<String>,
}

#[derive(Default)]
struct ImageTagger {
    images: Vec<ImageData>,
    image_cache: HashMap<usize, egui::TextureHandle>,
    current_dir: Option<PathBuf>,
    current_image_idx: usize,
    new_tag: String,
    progress: f32,
    activation_tag: String,
    modified_files: HashMap<PathBuf, bool>,
    currently_caching: Arc<Mutex<HashSet<usize>>>,
}

impl ImageTagger {
    pub fn new(_ctx: &Context) -> Self {
        Self::default()
    }

    pub fn load_directory(&mut self, path: &Path) {
        self.images.clear();
        self.image_cache.clear();
        self.progress = 0.0;

        let image_paths: Vec<_> = WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| {
                        let ext = ext.to_string_lossy().to_lowercase();
                        ext == "jpg" || ext == "jpeg" || ext == "png"
                    })
                    .unwrap_or(false)
            })
            .map(|entry| entry.path().to_path_buf())
            .collect();

        for path in &image_paths {
            let tags = self.load_tags_for_image(path).unwrap_or_default();
            self.images.push(ImageData { path: path.clone(), tags });
        }

        self.current_dir = Some(path.to_path_buf());
        self.start_caching();
    }

    fn load_tags_for_image(&self, path: &Path) -> Result<Vec<String>, std::io::Error> {
        let tags_path = path.with_extension("txt");
        if tags_path.exists() {
            let content = fs::read_to_string(tags_path)?;
            Ok(content
                .split(',')
                .map(|s| s.trim().to_string())
                .collect())
        } else {
            Ok(Vec::new())
        }
    }

    fn save_tags_for_image(&self, image: &ImageData) -> Result<(), std::io::Error> {
        let tags_path = image.path.with_extension("txt");
        fs::write(tags_path, image.tags.join(", "))
    }

    fn start_caching(&mut self) {
        let images = self.images.clone();
        let currently_caching = self.currently_caching.clone();
        let total_images = images.len();

        std::thread::spawn(move || {
            for (idx, image) in images.into_iter().enumerate() {
                {
                    let mut set = currently_caching.lock().unwrap();
                    if set.contains(&idx) {
                        continue;
                    }
                    set.insert(idx);
                }

                // Simulate caching (replace with actual image processing logic)
                std::thread::sleep(std::time::Duration::from_millis(50));

                {
                    let mut set = currently_caching.lock().unwrap();
                    set.remove(&idx);
                }

                println!("Cached image {}/{}", idx + 1, total_images);
            }
        });
    }

    fn add_activation_tag(&mut self) {
        for image in &mut self.images {
            if !image.tags.contains(&self.activation_tag) {
                image.tags.push(self.activation_tag.clone());
                self.modified_files.insert(image.path.clone(), true);
            }
        }
    }
}

impl eframe::App for ImageTagger {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Add Activation Tag").clicked() {
                    self.add_activation_tag();
                }
                ui.text_edit_singleline(&mut self.activation_tag);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Caching Progress:");
            ui.add(egui::ProgressBar::new(self.progress));
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions::default();
    eframe::run_native("Image Tagger", options, Box::new(|cc| Box::new(ImageTagger::new(&cc.egui_ctx))))
}
