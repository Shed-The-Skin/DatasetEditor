use eframe::egui;
use image::ImageReader;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use rfd::FileDialog;
use std::sync::{Arc, Mutex};
use std::thread;
use rayon::prelude::*;
#[path = "booru-tag-manager.rs"]
mod booru_tag_manager;

use booru_tag_manager::BooruTagManager;

#[derive(Serialize, Deserialize, Clone)]
struct ImageData {
    path: PathBuf,
    tags: Vec<String>,
    hash: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
enum SortType {
    AlphabeticalAsc,
    AlphabeticalDesc,
    FrequencyHighLow,
    FrequencyLowHigh,
}
#[derive(Debug)]
enum TagAction {
    Add(String),
    Remove(String),
}

#[derive(Clone)]
enum CacheMessage {
    ImageDecoded {
        idx: usize,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        hash: Vec<u8>,
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

#[derive(Clone)]
enum DuplicateMessage {
    Found { duplicates: HashMap<PathBuf, Vec<PathBuf>> },
}

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
    feedback_duration: f32,
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
    duplicate_images: HashMap<PathBuf, Vec<PathBuf>>,
    images_to_delete: HashSet<PathBuf>,
    feedback_tx: Option<std::sync::mpsc::Sender<DuplicateMessage>>,
    duplicate_rx: Option<std::sync::mpsc::Receiver<DuplicateMessage>>,
    booru_manager: BooruTagManager,
    show_tag_suggestions: bool,
    current_sort_type: Option<SortType>,
    right_panel_width: Option<f32>,
}

impl Default for ImageTagger {
    fn default() -> Self {
        Self {
            current_dir: None,
            images: Vec::new(),
            current_image_idx: 0,
            new_tag: String::new(),
            search_tag: String::new(),
            prepend_tags: false,
            sort_ascending: true,
            sort_by_frequency: false,
            current_texture: None,
            tag_to_remove: None,
            show_tag_stats: false,
            modified_files: HashMap::new(),
            feedback_message: None,
            feedback_timer: None,
            feedback_duration: 5.0,
            current_sorting: None,
            image_cache: HashMap::new(),
            last_logged_image_idx: None,
            state_changed: false,
            decoded_receiver: None,
            cache_progress: 0.0,
            total_images_to_cache: 0,
            cached_images_count: Arc::new(Mutex::new(0)),
            is_caching: false,
            activation_tag: String::new(),
            progress_receiver: None,
            duplicate_images: HashMap::new(),
            images_to_delete: HashSet::new(),
            feedback_tx: None,
            duplicate_rx: None,
            booru_manager: BooruTagManager::new(),
            show_tag_suggestions: false,
            current_sort_type: None,
            right_panel_width: Some(300.0), // Initialize with default width
        }
    }
}


impl ImageTagger {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut tagger = Self::default();

        if let Err(err) = tagger.booru_manager.load_from_csv(
            std::path::Path::new("danbooru-12-10-24-underscore.csv")
        ) {
            eprintln!("Failed to load Booru database: {}", err);
        }

        tagger
    }

    fn apply_current_sorting(&mut self) {
        if let Some(sort_type) = self.current_sort_type {
            if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                match sort_type {
                    SortType::AlphabeticalAsc => {
                        current_image.tags.sort();
                    }
                    SortType::AlphabeticalDesc => {
                        current_image.tags.sort_by(|a, b| b.cmp(a));
                    }
                    SortType::FrequencyHighLow => {
                        // Get frequencies before mutably borrowing image
                        let frequencies: HashMap<_, _> = current_image.tags.iter()
                            .fold(HashMap::new(), |mut map, tag| {
                                *map.entry(tag.to_string()).or_insert(0) += 1;
                                map
                            });
                        current_image.tags.sort_by(|a, b| {
                            frequencies.get(b).cmp(&frequencies.get(a))
                        });
                    }
                    SortType::FrequencyLowHigh => {
                        // Get frequencies before mutably borrowing image
                        let frequencies: HashMap<_, _> = current_image.tags.iter()
                            .fold(HashMap::new(), |mut map, tag| {
                                *map.entry(tag.to_string()).or_insert(0) += 1;
                                map
                            });
                        current_image.tags.sort_by(|a, b| {
                            frequencies.get(a).cmp(&frequencies.get(b))
                        });
                    }
                }
            }
        }
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
                    let _ = progress_tx.send(CacheProgress::Started { idx });

                    if let Some(image) = images.get(idx) {
                        let start = std::time::Instant::now();
                        let _ = progress_tx.send(CacheProgress::Loading { idx });

                        match ImageReader::open(&image.path) {
                            Ok(img_reader) => {
                                match img_reader.decode() {
                                    Ok(img) => {
                                        let small = img.resize(8, 8, image::imageops::FilterType::Nearest);
                                        let gray = small.grayscale();
                                        let buffer = gray.to_luma8();
                                        let pixels = buffer.as_raw();
                                        let average: u8 = (pixels.iter().map(|&p| p as u32).sum::<u32>() / pixels.len() as u32) as u8;

                                        let mut hash = Vec::with_capacity(8);
                                        for chunk in pixels.chunks(8) {
                                            let mut byte = 0u8;
                                            for (i, &pixel) in chunk.iter().enumerate() {
                                                if pixel > average {
                                                    byte |= 1 << i;
                                                }
                                            }
                                            hash.push(byte);
                                        }

                                        let width = (800.0 * (img.width() as f32 / img.height() as f32)).min(800.0) as u32;
                                        let height = (800.0 * (img.height() as f32 / img.width() as f32)).min(800.0) as u32;

                                        let resized = img.resize_exact(width, height, image::imageops::FilterType::Nearest);
                                        let rgba = resized.to_rgba8();

                                        if tx.send(CacheMessage::ImageDecoded {
                                            idx,
                                            width,
                                            height,
                                            pixels: rgba.to_vec(),
                                            hash,
                                        }).is_ok() {
                                            println!("Decoded in {:?}: {}", start.elapsed(), image.path.display());
                                            let mut count = cached_count.lock().unwrap();
                                            *count += 1;
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

    fn update_app(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process duplicate detection results
        if let Some(rx) = &self.duplicate_rx {
            if let Ok(DuplicateMessage::Found { duplicates }) = rx.try_recv() {
                self.duplicate_images = duplicates;
                let count = self.duplicate_images.values()
                    .map(|v| v.len())
                    .sum::<usize>();

                self.feedback_message = Some(format!(
                    "Found {} duplicate images. Click 'Remove Duplicates' to delete them.",
                    count
                ));
                self.feedback_timer = Some(std::time::Instant::now());
            }
        }

        // Process cached images
        if let Some(rx) = &self.decoded_receiver {
            while let Ok(message) = rx.try_recv() {
                match message {
                    CacheMessage::ImageDecoded { idx, width, height, pixels, hash } => {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [width as _, height as _],
                            &pixels,
                        );

                        let texture = ctx.load_texture(
                            format!("image_{}", idx),
                            color_image,
                            egui::TextureOptions::default(),
                        );

                        if let Some(image) = self.images.get_mut(idx) {
                            image.hash = Some(hash);
                        }

                        self.image_cache.insert(idx, texture);
                        let count = *self.cached_images_count.lock().unwrap();
                        self.cache_progress = count as f32 / self.total_images_to_cache as f32;
                        ctx.request_repaint();
                    }
                    CacheMessage::Error { idx, error } => {
                        eprintln!("Error caching image {}: {}", idx, error);
                    }
                }
            }
        }

        // Draw UI panels
        self.draw_top_panel(ctx);
        self.draw_left_panel(ctx);
        self.draw_central_panel(ctx);
        self.draw_right_panel(ctx);
    }

    fn draw_top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.draw_feedback_message(ui);

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
    }

    fn draw_left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("image_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open Directory").clicked() {
                    if let Some(path) = FileDialog::new().pick_folder() {
                        self.load_directory(ctx, &path);
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
    }

    fn draw_feedback_message(&mut self, ui: &mut egui::Ui) {
        if let Some(timer) = self.feedback_timer {
            let elapsed = timer.elapsed().as_secs_f32();

            if elapsed < self.feedback_duration {
                let alpha = if elapsed > (self.feedback_duration - 3.0) {
                    ((self.feedback_duration - elapsed) / 3.0).clamp(0.0, 1.0)
                } else {
                    1.0
                };

                if let Some(message) = &self.feedback_message {
                    let color = egui::Color32::from_rgba_unmultiplied(
                        0,    // Red
                        255,  // Green
                        0,    // Blue
                        (alpha * 255.0) as u8  // Alpha
                    );
                    ui.colored_label(color, message);
                }
                ui.ctx().request_repaint();
            } else {
                self.feedback_message = None;
                self.feedback_timer = None;
            }
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

    fn backup_dataset(&mut self) {
        if let Some(dir) = &self.current_dir {
            let backup_dir = dir.join("backup");

            self.pause_caching();
            let current_texture = self.current_texture.clone();

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
                    self.feedback_message = Some(format!("Error: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }
            }

            if let Err(err) = fs::create_dir_all(&backup_dir) {
                self.feedback_message = Some(format!("Error during backup creation: {}", err));
                self.feedback_timer = Some(std::time::Instant::now());
                self.resume_caching();
                return;
            }

            for image in &self.images {
                let tags_path = image.path.with_extension("txt");

                if let Err(err) = fs::copy(&image.path, backup_dir.join(image.path.file_name().unwrap())) {
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    self.resume_caching();
                    return;
                }

                if let Err(err) = fs::copy(&tags_path, backup_dir.join(tags_path.file_name().unwrap())) {
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
    fn get_tag_frequencies_for_image(&self, image: &ImageData) -> HashMap<String, usize> {
        let mut freq_map: HashMap<String, usize> = HashMap::new();
        for tag in &image.tags {
            *freq_map.entry(tag.clone()).or_insert(0) += 1;
        }
        freq_map
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

    fn save_tags_for_image(&self, image_data: &ImageData) -> Result<(), std::io::Error> {
        let tags_path = image_data.path.with_extension("txt");
        fs::write(tags_path, image_data.tags.join(", "))
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

    fn change_image(&mut self, ctx: &egui::Context) {
        if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
            self.current_texture = Some(texture);
        } else {
            self.load_image_texture(ctx);
        }
        // Apply sorting after changing image
        self.apply_current_sorting();
    }

    fn apply_activation_tag(&mut self) {
        if !self.activation_tag.is_empty() {
            for image in &mut self.images {
                if !image.tags.contains(&self.activation_tag) {
                    image.tags.insert(0, self.activation_tag.clone());
                    self.modified_files.insert(image.path.clone(), true);
                }
            }
            self.feedback_message = Some("Activation tag applied to all images".to_string());
            self.feedback_timer = Some(std::time::Instant::now());
        }
    }

    fn previous_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + self.images.len() - 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn next_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn remove_duplicates_for_all(&mut self) {
        let images = &mut self.images;
        for image in images.iter_mut() {
            let mut seen = std::collections::HashSet::new();
            image.tags.retain(|tag| seen.insert(tag.clone()));
            self.modified_files.insert(image.path.clone(), true);
        }
        self.feedback_message = Some("Removed duplicate tags from all images".to_string());
        self.feedback_timer = Some(std::time::Instant::now());
    }

    fn process_tags_text(text: &str) -> Vec<String> {
        text.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }


    fn draw_central_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(current_image) = self.images.get(self.current_image_idx).cloned() {
                ui.vertical(|ui| {
                    // Section heading
                    ui.heading("Tags for Current Image");

                    // Sorting controls
                    ui.horizontal(|ui| {
                        if ui.button("Sort Alphabetically (A-Z)").clicked() {
                            self.current_sort_type = Some(SortType::AlphabeticalAsc);
                            self.apply_current_sorting();
                        }
                        if ui.button("Sort Alphabetically (Z-A)").clicked() {
                            self.current_sort_type = Some(SortType::AlphabeticalDesc);
                            self.apply_current_sorting();
                        }
                        if ui.button("Sort by Frequency (High-Low)").clicked() {
                            self.current_sort_type = Some(SortType::FrequencyHighLow);
                            self.apply_current_sorting();
                        }
                        if ui.button("Sort by Frequency (Low-High)").clicked() {
                            self.current_sort_type = Some(SortType::FrequencyLowHigh);
                            self.apply_current_sorting();
                        }
                    });

                    // Calculate available width for the middle panel
                    let total_width = ui.available_width();
                    let right_panel_width = self.right_panel_width.unwrap_or(300.0);
                    let buffer = 20.0; // Add a buffer to prevent overlap
                    let middle_panel_width = total_width - right_panel_width - buffer;

                    // Wrapping tags
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0); // Add spacing between tags
                            ui.set_width(middle_panel_width); // Restrict width to middle panel
                            for tag in &current_image.tags {
                                // Replace spaces with non-breaking spaces to prevent word wrapping
                                let non_breaking_tag = tag.replace(' ', "\u{00A0}");
                                ui.label(non_breaking_tag);
                            }
                        });
                    });
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("No tags to display.");
                });
            }
        });
    }

    fn draw_right_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("tag_panel")
            .resizable(true)
            .min_width(300.0)
            .default_width(300.0)
            .max_width(800.0)
            .show_separator_line(true)
            .show(ctx, |ui| {
                // Dynamically store the current width of the right panel
                self.right_panel_width = Some(ui.available_width());

                // Panel heading
                ui.heading("Tag Editing");


                // Add Booru tag section
                ui.group(|ui| {
                    ui.heading("Add Booru Tag");
                    if let Some(selected_tag) = self.booru_manager.draw_tag_editor(ui) {
                        println!("Attempting to add tag to current image: {}", selected_tag);

                        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                            if !current_image.tags.contains(&selected_tag) {
                                current_image.tags.push(selected_tag.clone());
                                self.modified_files.insert(current_image.path.clone(), true);

                                println!("Tag added successfully! Current tags: {:?}", current_image.tags);
                            } else {
                                println!("Tag already exists: {}", selected_tag);
                            }
                        } else {
                            println!("No current image available to add the tag.");
                        }
                    }


                });

                ui.add_space(10.0);
                ui.separator();

                // Tag management controls
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
                ui.separator();

                if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                    let mut tags_text = current_image.tags.join(", ");
                    let text_edit = egui::TextEdit::multiline(&mut tags_text)
                        .desired_width(ui.available_width())
                        .font(egui::TextStyle::Monospace)
                        .cursor_at_end(true)
                        .lock_focus(false);

                    if ui.add(text_edit).changed() {
                        let new_tags = tags_text
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>();
                        current_image.tags = new_tags;
                        self.modified_files.insert(current_image.path.clone(), true);
                    }
                }


            });
    }
    fn load_directory(&mut self, ctx: &egui::Context, path: &Path) {
        self.images.clear();
        self.image_cache.clear();
        self.current_image_idx = 0;
        self.cache_progress = 0.0;
        self.is_caching = false;
        *self.cached_images_count.lock().unwrap() = 0;

        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() && matches!(path.extension().and_then(|e| e.to_str()),
                    Some("jpg" | "jpeg" | "png")) {
                    let tags = self.load_tags_for_image(&path).unwrap_or_default();
                    self.images.push(ImageData {
                        path,
                        tags,
                        hash: None, // Initialize hash as None
                    });
                }
            }
        }

        println!("Starting background caching for {} images...", self.images.len());
        self.current_dir = Some(path.to_path_buf());
        self.total_images_to_cache = self.images.len();
        self.is_caching = true;

        self.start_background_caching();

        if !self.images.is_empty() {
            self.current_image_idx = 0;
            self.load_image_texture(ctx);
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

    fn draw_tag_list(&mut self, ui: &mut egui::Ui) {
        if let Some(current_image) = self.images.get(self.current_image_idx).cloned() {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0); // Adjust spacing between tags
                        for tag in &current_image.tags {
                            if !tag.is_empty() { // Skip empty tags
                                ui.group(|ui| {
                                    ui.horizontal(|ui| {
                                        if let Some(tag_type) = self.booru_manager.get_tag_type(tag) {
                                            let color = match tag_type {
                                                0 => egui::Color32::GRAY,
                                                1 => egui::Color32::RED,
                                                3 => egui::Color32::GREEN,
                                                4 => egui::Color32::YELLOW,
                                                _ => egui::Color32::WHITE,
                                            };
                                            ui.colored_label(color, "●");
                                            ui.add_space(4.0);
                                        }
                                        ui.label(egui::RichText::new(tag).size(16.0));
                                    });
                                });
                            }
                        }
                    });
                });

        }
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
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.update_app(ctx, frame);
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
        Box::new(|cc| Ok(Box::new(ImageTagger::new(cc)))), // Fix: Wrap in Ok()
    )
}