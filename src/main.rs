use eframe::egui;
use image::io::Reader as ImageReader;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use rfd::FileDialog;
use std::sync::{Arc, Mutex};
use std::thread;
use rayon::prelude::*;

#[derive(Serialize, Deserialize, Clone)]
struct ImageData {
    path: PathBuf,
    tags: Vec<String>,
}

#[derive(Debug)]
enum TagAction {
    Add(String),
    Remove(String),
}

#[derive(Clone)]
struct CacheUpdate {
    idx: usize,
    texture: egui::TextureHandle,
}

#[derive(Clone)]
struct DecodedImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    path: PathBuf,
}

#[derive(Clone)]
enum CacheMessage {
    ImageDecoded {
        idx: usize,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Error {
        idx: usize,
        error: String,
    },
}

#[derive(Clone)]
enum CacheProgress {
    Started { idx: usize },
    Loading { idx: usize },
    Resizing { idx: usize },
    Completed { idx: usize },
    Error { idx: usize },
}

#[derive(Default)]
struct ImageTagger {
    current_dir: Option<PathBuf>,
    images: Vec<ImageData>,
    current_image_idx: usize,
    new_tag: String,
    search_tag: String,
    prepend_tags: bool,
    sort_ascending: bool,
    sort_by_frequency: bool,
    current_texture: Option<egui::TextureHandle>,
    tag_to_remove: Option<String>,
    show_tag_stats: bool,
    modified_files: HashMap<PathBuf, bool>,
    feedback_message: Option<String>,
    feedback_timer: Option<std::time::Instant>,
    current_sorting: Option<fn(&String, &String) -> std::cmp::Ordering>,
    image_cache: HashMap<usize, egui::TextureHandle>,
    last_logged_image_idx: Option<usize>,
    state_changed: bool,
    decoded_receiver: Option<std::sync::mpsc::Receiver<CacheMessage>>,
    cache_progress: f32,
    total_images_to_cache: usize,
    cached_images_count: Arc<Mutex<usize>>,
    is_caching: bool,
    activation_tag: String,
    progress_receiver: Option<std::sync::mpsc::Receiver<CacheProgress>>,
}

impl ImageTagger {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            progress_receiver: None,
            decoded_receiver: None,
            cache_progress: 0.0,
            total_images_to_cache: 0,
            cached_images_count: Arc::new(Mutex::new(0)),
            is_caching: false,
            activation_tag: String::new(),
            ..Default::default()
        }
    }

    fn start_background_caching(&mut self) {
        let total_images = self.images.len();
        if total_images == 0 {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        self.decoded_receiver = Some(rx);
        self.progress_receiver = Some(progress_rx);

        let images = self.images.clone();
        let cached_count = self.cached_images_count.clone();

        thread::spawn(move || {
            let chunk_size = 10;
            for chunk_start in (0..total_images).step_by(chunk_size) {
                let chunk_end = (chunk_start + chunk_size).min(total_images);
                let chunk_indices: Vec<_> = (chunk_start..chunk_end).collect();

                chunk_indices.into_par_iter().for_each_with((tx.clone(), progress_tx.clone()), |(tx, progress_tx), idx| {
                    // Send started status
                    let _ = progress_tx.send(CacheProgress::Started { idx });

                    if let Some(image) = images.get(idx) {
                        let start = std::time::Instant::now();

                        // Send loading status
                        let _ = progress_tx.send(CacheProgress::Loading { idx });

                        match ImageReader::open(&image.path) {
                            Ok(img_reader) => {
                                match img_reader.decode() {
                                    Ok(img) => {
                                        // Send resizing status
                                        let _ = progress_tx.send(CacheProgress::Resizing { idx });

                                        let width = (800.0 * (img.width() as f32 / img.height() as f32)).min(800.0) as u32;
                                        let height = (800.0 * (img.height() as f32 / img.width() as f32)).min(800.0) as u32;

                                        let resized = img.resize_exact(width, height, image::imageops::FilterType::Nearest);
                                        let rgba = resized.to_rgba8();

                                        if tx.send(CacheMessage::ImageDecoded {
                                            idx,
                                            width,
                                            height,
                                            pixels: rgba.to_vec(),
                                        }).is_ok() {
                                            println!("Decoded in {:?}: {}", start.elapsed(), image.path.display());

                                            let mut count = cached_count.lock().unwrap();
                                            *count += 1;

                                            // Send completed status
                                            let _ = progress_tx.send(CacheProgress::Completed { idx });
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(CacheMessage::Error {
                                            idx,
                                            error: format!("Decode error: {}", e),
                                        });
                                        let _ = progress_tx.send(CacheProgress::Error { idx });
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(CacheMessage::Error {
                                    idx,
                                    error: format!("Open error: {}", e),
                                });
                                let _ = progress_tx.send(CacheProgress::Error { idx });
                            }
                        }
                    }
                });
            }
        });
    }

    fn pause_caching(&mut self) {
        self.is_caching = false;
        // Preserve receiver and cache, just pause the process
    }

    fn resume_caching(&mut self) {
        if !self.images.is_empty() && !self.is_caching {
            self.is_caching = true;
            // Don't restart caching if we still have the receiver
            if self.decoded_receiver.is_none() {
                self.start_background_caching();
            }
        }
    }

    fn remove_tag_from_all(&mut self, tag: &str) {
        let mut modified_count = 0;
        for image in &mut self.images {
            if image.tags.contains(&tag.to_string()) {
                image.tags.retain(|t| t != tag);
                self.modified_files.insert(image.path.clone(), true);
                modified_count += 1;
            }
        }
        self.feedback_message = Some(format!("Removed tag '{}' from {} images", tag, modified_count));
        self.feedback_timer = Some(std::time::Instant::now());
    }

    fn apply_activation_tag(&mut self) {
        if !self.activation_tag.is_empty() {
            for image in &mut self.images {
                if !image.tags.contains(&self.activation_tag) {
                    image.tags.push(self.activation_tag.clone());
                    self.modified_files.insert(image.path.clone(), true);
                }
            }
            self.feedback_message = Some("Activation tag applied to all images".to_string());
            self.feedback_timer = Some(std::time::Instant::now());
        }
    }

    fn load_image_texture(&mut self, ctx: &egui::Context) -> bool {
        if let Some(current_image) = self.images.get(self.current_image_idx) {
            // Check cache first
            if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
                println!("Loading image from cache: {}", current_image.path.display());
                self.current_texture = Some(texture);
                return true;
            }

            let file_size = fs::metadata(&current_image.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);

            println!("❗ Loading non-cached image: {} (Size: {} KB) ❗",
                     current_image.path.display(), file_size / 1024);

            if let Ok(img_reader) = ImageReader::open(&current_image.path) {
                if let Ok(img) = img_reader.decode() {
                    let start = std::time::Instant::now();
                    let resized_img = img.resize(800, 800, image::imageops::FilterType::Triangle);

                    let size = [resized_img.width() as _, resized_img.height() as _];
                    let image_buffer = resized_img.to_rgba8();
                    let pixels = image_buffer.as_flat_samples();

                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        size,
                        pixels.as_slice(),
                    );

                    let texture = ctx.load_texture(
                        format!("image_{}", self.current_image_idx),
                        color_image,
                        egui::TextureOptions::default(),
                    );

                    println!("Loaded in {:?}", start.elapsed());

                    self.current_texture = Some(texture.clone());
                    self.image_cache.insert(self.current_image_idx, texture);

                    return true;
                }
            }
        }
        false
    }

    fn change_image(&mut self, ctx: &egui::Context) {
        if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
            println!("Loading image from cache: {}",
                     self.images[self.current_image_idx].path.display());
            self.current_texture = Some(texture);
        } else {
            self.load_image_texture(ctx);
        }
    }

    fn remove_duplicates_for_image(&mut self, image: &mut ImageData) {
        let mut seen = std::collections::HashSet::new();
        image.tags.retain(|tag| seen.insert(tag.clone()));
        self.modified_files.insert(image.path.clone(), true);
    }

    fn remove_duplicates_for_all(&mut self) {
        let images = &mut self.images;
        for image in images.iter_mut() {
            let mut seen = std::collections::HashSet::new();
            image.tags.retain(|tag| seen.insert(tag.clone()));
            self.modified_files.insert(image.path.clone(), true);
        }
    }

    fn apply_current_sorting(&mut self) {
        if let Some(sort_fn) = self.current_sorting {
            self.images[self.current_image_idx]
                .tags
                .sort_by(sort_fn);
        }
    }

    fn next_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn previous_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + self.images.len() - 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn backup_dataset(&mut self) {
        if let Some(dir) = &self.current_dir {
            let backup_dir = dir.join("backup");

            // Pause caching but preserve current texture
            self.pause_caching();
            let current_texture = self.current_texture.clone();

            // Check if the backup directory exists
            if backup_dir.exists() {
                let result = rfd::MessageDialog::new()
                    .set_title("Backup Confirmation")
                    .set_description("The backup folder already exists. Do you want to replace it?")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();

                if result == rfd::MessageDialogResult::No {
                    self.feedback_message = Some("Backup cancelled by the user.".to_string());
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }

                if let Err(err) = fs::remove_dir_all(&backup_dir) {
                    eprintln!("Failed to delete existing backup directory: {}", err);
                    self.feedback_message = Some(format!("Error: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }
            }

            if let Err(err) = fs::create_dir_all(&backup_dir) {
                eprintln!("Failed to create backup directory: {}", err);
                self.feedback_message = Some(format!("Error during backup creation: {}", err));
                self.feedback_timer = Some(std::time::Instant::now());
                self.resume_caching();
                return;
            }

            for image in &self.images {
                let tags_path = image.path.with_extension("txt");

                if let Err(err) = fs::copy(&image.path, backup_dir.join(image.path.file_name().unwrap())) {
                    eprintln!("Failed to backup image {}: {}", image.path.display(), err);
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }

                if let Err(err) = fs::copy(&tags_path, backup_dir.join(tags_path.file_name().unwrap())) {
                    eprintln!("Failed to backup tags {}: {}", tags_path.display(), err);
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }
            }

            self.current_texture = current_texture;
            self.feedback_message = Some("Backup completed successfully!".to_string());
            self.feedback_timer = Some(std::time::Instant::now());
            self.resume_caching();
        }
    }

    fn save_all(&mut self) {
        for image in &self.images {
            if *self.modified_files.get(&image.path).unwrap_or(&false) {
                if let Err(err) = self.save_tags_for_image(image) {
                    eprintln!("Failed to save tags for {}: {}", image.path.display(), err);
                    self.feedback_message = Some(format!("Error saving tags: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    return;
                }
            }
        }
        self.feedback_message = Some("All changes saved successfully!".to_string());
        self.feedback_timer = Some(std::time::Instant::now());
    }

    fn get_tag_frequencies_for_image(&self, image: &ImageData) -> HashMap<String, usize> {
        let mut freq_map: HashMap<String, usize> = HashMap::new();
        for tag in &image.tags {
            *freq_map.entry(tag.clone()).or_insert(0) += 1;
        }
        freq_map
    }

    fn load_directory(&mut self, path: &Path) {
        self.images.clear();
        self.image_cache.clear();
        self.current_image_idx = 0;
        self.cache_progress = 0.0;
        self.is_caching = false;
        *self.cached_images_count.lock().unwrap() = 0;

        // Load image paths and metadata from only the selected directory (no subdirs)
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() && matches!(path.extension().and_then(|e| e.to_str()),
                    Some("jpg" | "jpeg" | "png")) {
                    let tags = self.load_tags_for_image(&path).unwrap_or_default();
                    self.images.push(ImageData { path, tags });
                }
            }
        }

        println!("Starting background caching for {} images...", self.images.len());
        self.current_dir = Some(path.to_path_buf());
        self.total_images_to_cache = self.images.len(); // Set total before starting
        self.is_caching = true;  // Set caching flag before starting

        // Start the background caching process
        self.start_background_caching();
    }

    fn load_tags_for_image(&self, image_path: &Path) -> Result<Vec<String>, std::io::Error> {
        let tags_path = image_path.with_extension("txt");
        if tags_path.exists() {
            let content = fs::read_to_string(tags_path)?;
            Ok(content
                .split(',')
                .map(|tag| tag.trim().to_string())
                .filter(|tag| !tag.is_empty())
                .collect())
        } else {
            Ok(Vec::new())
        }
    }

    fn save_tags_for_image(&self, image_data: &ImageData) -> Result<(), std::io::Error> {
        let tags_path = image_data.path.with_extension("txt");
        fs::write(tags_path, image_data.tags.join(", "))
    }

    fn handle_tag_addition_for_image(&mut self, tag: String) {
        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
            if self.prepend_tags {
                current_image.tags.insert(0, tag);
            } else {
                current_image.tags.push(tag);
            }
            let mut seen = std::collections::HashSet::new();
            current_image.tags.retain(|t| seen.insert(t.clone()));
            self.modified_files.insert(current_image.path.clone(), true);
        }
    }

    fn handle_tag_removal_for_image(&mut self, tag: String) {
        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
            current_image.tags.retain(|t| t != &tag);
            self.modified_files.insert(current_image.path.clone(), true);
        }
    }
}

impl eframe::App for ImageTagger {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process decoded messages
        if let Some(rx) = &self.decoded_receiver {
            while let Ok(message) = rx.try_recv() {
                match message {
                    CacheMessage::ImageDecoded { idx, width, height, pixels } => {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [width as _, height as _],
                            &pixels,
                        );

                        let texture = ctx.load_texture(
                            format!("image_{}", idx),
                            color_image,
                            egui::TextureOptions::default(),
                        );

                        self.image_cache.insert(idx, texture);
                        // Update progress whenever we cache an image
                        self.cache_progress = *self.cached_images_count.lock().unwrap() as f32 / self.total_images_to_cache as f32;
                        ctx.request_repaint();
                        println!("Cache size is now: {} images", self.image_cache.len());
                    }
                    CacheMessage::Error { idx, error } => {
                        eprintln!("Error caching image {}: {}", idx, error);
                    }
                }
            }
        }

        // Top panel with controls and progress
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            if let Some(message) = &self.feedback_message {
                ui.colored_label(egui::Color32::GREEN, message);
            }

            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.save_all();
                }
                if ui.button("Backup").clicked() {
                    self.backup_dataset();
                }

                ui.separator();
                ui.label("Activation tag:");
                if ui.text_edit_singleline(&mut self.activation_tag).lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.apply_activation_tag();
                }
                if ui.button("Apply").clicked() {
                    self.apply_activation_tag();
                }
            });

            // Add progress bar in its own row
            if self.is_caching {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::ProgressBar::new(self.cache_progress)
                            .show_percentage()
                            .desired_width(ui.available_width())
                    );
                });
            }
        });

        // Left panel with image display
        egui::SidePanel::left("image_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open Directory").clicked() {
                    if let Some(path) = FileDialog::new().pick_folder() {
                        self.load_directory(&path);
                    }
                }
            });

            if let Some(dir) = &self.current_dir {
                ui.label(format!("Current directory: {}", dir.display()));
            }

            ui.separator();

            if !self.images.is_empty() {
                ui.horizontal(|ui| {
                    if ui.button("Previous").clicked() {
                        self.previous_image(ctx);
                    }
                    if ui.button("Next").clicked() {
                        self.next_image(ctx);
                    }
                    ui.label(format!("Image {}/{}", self.current_image_idx + 1, self.images.len()));
                });
                ui.separator();
            }

            if let Some(texture) = &self.current_texture {
                let available_size = ui.available_size();
                let aspect_ratio = texture.aspect_ratio();
                let mut size = available_size;

                if (size.x / size.y) > aspect_ratio {
                    size.x = size.y * aspect_ratio;
                } else {
                    size.y = size.x / aspect_ratio;
                }

                ui.add(egui::Image::new(texture)
                    .fit_to_original_size(1.0)
                    .max_size(size));
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("No image loaded. Please select a directory.");
                });
            }
        });

        // Central panel for displaying tags
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(current_image) = self.images.get(self.current_image_idx).cloned() {
                ui.heading("Tags for Current Image");

                let tag_frequencies = self.get_tag_frequencies_for_image(&current_image);

                ui.horizontal(|ui| {
                    if ui.button("Sort Alphabetically (A-Z)").clicked() {
                        self.current_sorting = Some(|a, b| a.cmp(b));
                        self.images[self.current_image_idx].tags.sort();
                    }
                    if ui.button("Sort Alphabetically (Z-A)").clicked() {
                        self.current_sorting = Some(|a, b| b.cmp(a));
                        self.images[self.current_image_idx].tags.sort_by(|a, b| b.cmp(a));
                    }
                    if ui.button("Sort by Frequency (High-Low)").clicked() {
                        self.images[self.current_image_idx].tags.sort_by(|a, b| {
                            tag_frequencies.get(b).cmp(&tag_frequencies.get(a))
                        });
                    }
                    if ui.button("Sort by Frequency (Low-High)").clicked() {
                        self.images[self.current_image_idx].tags.sort_by(|a, b| {
                            tag_frequencies.get(a).cmp(&tag_frequencies.get(b))
                        });
                    }
                });

                egui::ScrollArea::vertical()
                    .id_source(format!("tag_display_{}", self.current_image_idx))
                    .show(ui, |ui| {
                        for tag in &self.images[self.current_image_idx].tags {
                            ui.label(tag);
                        }
                    });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("No tags to display.");
                });
            }
        });

        // Right panel for tag editing
        egui::SidePanel::right("tag_panel")
            .resizable(false)
            .min_width(300.0)
            .default_width(300.0)
            .show(ctx, |ui| {
                let mut tag_action = None;

                ui.heading("Tag Editing");

                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if ui.button("Remove Duplicates (Current)").clicked() {
                        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                            let mut seen = std::collections::HashSet::new();
                            current_image.tags.retain(|tag| seen.insert(tag.clone()));
                            self.modified_files.insert(current_image.path.clone(), true);
                        }
                    }
                    if ui.button("Remove Duplicates (All)").clicked() {
                        self.remove_duplicates_for_all();
                    }
                });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    static mut REMOVE_TAG: String = String::new();
                    unsafe {
                        let text_response = ui.text_edit_singleline(&mut REMOVE_TAG);
                        if ui.button("Remove From All").clicked() && !REMOVE_TAG.is_empty() {
                            self.remove_tag_from_all(&REMOVE_TAG);
                            REMOVE_TAG.clear();
                        }
                        if text_response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !REMOVE_TAG.is_empty() {
                            self.remove_tag_from_all(&REMOVE_TAG);
                            REMOVE_TAG.clear();
                        }
                    }
                });

                ui.add_space(10.0);
                ui.separator();

                ui.horizontal(|ui| {
                    let _response = ui.add(
                        egui::TextEdit::singleline(&mut self.new_tag)
                            .desired_width(ui.available_width() - 60.0),
                    );
                    if ui.button("Add").clicked() && !self.new_tag.is_empty() {
                        self.handle_tag_addition_for_image(self.new_tag.clone());
                        self.new_tag.clear();
                    }
                    if _response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        if !self.new_tag.is_empty() {
                            self.handle_tag_addition_for_image(self.new_tag.clone());
                            self.new_tag.clear();
                        }
                    }
                });

                if let Some(_current_image) = self.images.get(self.current_image_idx) {
                    egui::ScrollArea::vertical()
                        .id_source(format!("tag_editing_{}", self.current_image_idx))
                        .show(ui, |ui| {
                            for tag in &self.images[self.current_image_idx].tags {
                                ui.horizontal(|ui| {
                                    ui.label(tag);
                                    if ui.small_button("×").clicked() {
                                        tag_action = Some(TagAction::Remove(tag.clone()));
                                    }
                                });
                            }
                        });
                }

                if let Some(action) = tag_action {
                    match action {
                        TagAction::Add(tag) => {
                            self.handle_tag_addition_for_image(tag);
                        }
                        TagAction::Remove(tag) => {
                            self.handle_tag_removal_for_image(tag);
                        }
                    }
                }
            });
    }
}

fn main() -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Image Tagger",
        native_options,
        Box::new(|cc| Ok(Box::new(ImageTagger::new(cc)))),
    )
}