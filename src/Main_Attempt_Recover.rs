use eframe::egui;
use image::io::Reader as ImageReader;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet}; // Added HashSet
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use rfd::FileDialog;
use std::sync::{Arc, Mutex};
use std::thread;

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
    modified_files: HashMap<PathBuf, bool>, // Tracks which files are modified
    feedback_message: Option<String>, // To store user feedback messages
    feedback_timer: Option<std::time::Instant>, // To handle feedback timeout
    current_sorting: Option<fn(&String, &String) -> std::cmp::Ordering>,
    image_cache: HashMap<usize, egui::TextureHandle>, // Cache for preloaded images
    last_logged_image_idx: Option<usize>, // Track the last logged image
    last_preloaded_index: Option<usize>,
    state_changed: bool,
    currently_caching: Arc<Mutex<HashSet<usize>>>,
    cache_update_receiver: Option<std::sync::mpsc::Receiver<CacheUpdate>>,
    loading_image: Arc<Mutex<Option<usize>>>, // Track which image is currently being loaded
    furthest_viewed_idx: usize, // Track the furthest image we've viewed
    forward_cache_position: Arc<Mutex<Option<usize>>>, // Track forward caching progress
    backward_cache_position: Arc<Mutex<Option<usize>>>,
    cache_progress: f32,  // Progress from 0.0 to 1.0
    total_images_to_cache: usize,
    cached_images_count: Arc<Mutex<usize>>,
    is_caching: bool,
    activation_tag: String,
}


impl ImageTagger {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            // ... existing fields ...
            cache_progress: 0.0,
            total_images_to_cache: 0,
            cached_images_count: Arc::new(Mutex::new(0)),
            is_caching: false,
            activation_tag: String::new(),
            ..Default::default()
        }
    }

    fn start_background_caching(&mut self, ctx: Arc<egui::Context>) {
        let total_images = self.images.len();
        if total_images == 0 {
            return;
        }

        self.total_images_to_cache = total_images;
        self.is_caching = true;
        *self.cached_images_count.lock().unwrap() = 0;

        let (tx, rx) = std::sync::mpsc::channel();
        self.cache_update_receiver = Some(rx);

        // Clone necessary fields for the background thread
        let images = self.images.clone();
        let ctx_clone = ctx;
        let currently_caching = self.currently_caching.clone();
        let loading_image = self.loading_image.clone();
        let cached_count = self.cached_images_count.clone();

        thread::spawn(move || {
            // Process images sequentially
            for idx in 0..total_images {
                // Skip if already cached or being processed
                {
                    let loading = loading_image.lock().unwrap();
                    let caching = currently_caching.lock().unwrap();
                    if Some(idx) == *loading || caching.contains(&idx) {
                        continue;
                    }
                }

                // Mark as being processed
                {
                    let mut caching_set = currently_caching.lock().unwrap();
                    caching_set.insert(idx);
                }

                if let Some(image) = images.get(idx) {
                    println!("Caching image {}/{}: {}",
                             idx + 1, total_images, image.path.display());

                    // Handle WSL2 paths
                    let path = if image.path.to_string_lossy().contains("\\wsl$") {
                        // Convert WSL path to native Windows path
                        image.path.to_string_lossy().replace("\\wsl$\\", "\\\\wsl$\\")
                    } else {
                        image.path.to_string_lossy().to_string()
                    };

                    if let Ok(img_reader) = ImageReader::open(&path) {
                        if let Ok(img) = img_reader.decode() {
                            let start = std::time::Instant::now();
                            let resized_img = img.resize(800, 800, image::imageops::FilterType::Triangle);
                            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                                [resized_img.width() as _, resized_img.height() as _],
                                resized_img.to_rgba8().as_flat_samples().as_slice(),
                            );

                            let texture = ctx_clone.load_texture(
                                format!("image_{}", idx),
                                color_image,
                                egui::TextureOptions::default(),
                            );

                            let update = CacheUpdate { idx, texture };
                            if tx.send(update).is_ok() {
                                // Update progress
                                let mut count = cached_count.lock().unwrap();
                                *count += 1;
                                println!("Cached {}/{} images", *count, total_images);
                            }
                        }
                    }
                }

                // Clear processing status
                {
                    let mut caching_set = currently_caching.lock().unwrap();
                    caching_set.remove(&idx);
                }
            }
        });
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

    fn get_canonical_path(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    // Remove get_canonical_path and update related functions to use paths directly
    fn load_image_texture(&mut self, ctx: &egui::Context) -> bool {
        if let Some(current_image) = self.images.get(self.current_image_idx) {
            // Check if it's already in cache first
            if let Some(texture) = self.image_cache.get(&self.current_image_idx).cloned() {
                println!("Loading image from cache: {}", current_image.path.display());
                self.current_texture = Some(texture);
                return true;
            }

            // Check if we're already loading this image
            {
                let mut loading = self.loading_image.lock().unwrap();
                if loading.is_some() {
                    return false;
                }
                *loading = Some(self.current_image_idx);
            }

            let start_time = std::time::Instant::now();
            let file_size = fs::metadata(&current_image.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);

            println!("❗ Loading non-cached image: {} (Size: {} KB) ❗",
                     current_image.path.display(), file_size / 1024);

            if let Ok(img_reader) = ImageReader::open(&current_image.path) {
                if let Ok(img) = img_reader.decode() {
                    let decode_time = start_time.elapsed();
                    println!("Image decoded in {:?}", decode_time);

                    let resized_img = img.resize(800, 800, image::imageops::FilterType::Triangle);
                    let resize_time = start_time.elapsed() - decode_time;
                    println!("Image resized in {:?}", resize_time);

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

                    self.current_texture = Some(texture.clone());
                    self.image_cache.insert(self.current_image_idx, texture);

                    println!("❗ Non-cached image loaded in {:?} ❗", start_time.elapsed());
                    println!("Cache size is now: {} images", self.image_cache.len());

                    // Clear loading state
                    let mut loading = self.loading_image.lock().unwrap();
                    *loading = None;

                    return true;
                }
            }

            // Clear loading state on error
            let mut loading = self.loading_image.lock().unwrap();
            *loading = None;
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

        let ctx = Arc::new(ctx.clone());
        self.trigger_preload(ctx);
    }


    fn remove_duplicates_for_image(&mut self, image: &mut ImageData) {
        let mut seen = std::collections::HashSet::new();
        image.tags.retain(|tag| seen.insert(tag.clone()));
        self.modified_files.insert(image.path.clone(), true); // Mark as modified
    }

    fn remove_duplicates_for_all(&mut self) {
        let images = &mut self.images; // Borrow images mutably
        for image in images.iter_mut() {
            // Iterate mutably over each image
            let mut seen = std::collections::HashSet::new();
            image.tags.retain(|tag| seen.insert(tag.clone()));
            self.modified_files.insert(image.path.clone(), true); // Mark as modified
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
            // Update furthest viewed position if we're moving forward
            if self.current_image_idx > self.furthest_viewed_idx {
                self.furthest_viewed_idx = self.current_image_idx;
            }
            self.change_image(ctx);
        }
    }

    fn previous_image(&mut self, ctx: &egui::Context) {
        if !self.images.is_empty() {
            self.current_image_idx = (self.current_image_idx + self.images.len() - 1) % self.images.len();
            self.change_image(ctx);
        }
    }

    fn trigger_preload(&mut self, ctx: Arc<egui::Context>) {
        let total_images = self.images.len();
        if total_images == 0 {
            return;
        }

        // Always cache ahead from the furthest viewed position
        let cache_start_idx = self.furthest_viewed_idx;
        let preload_indices: Vec<usize> = (1..=5)
            .map(|offset| (cache_start_idx + offset) % total_images)
            .collect();

        // Get indices that are neither cached nor currently being processed
        let missing_indices: Vec<usize> = {
            let currently_caching = self.currently_caching.lock().unwrap();
            let loading = self.loading_image.lock().unwrap();
            preload_indices
                .into_iter()
                .filter(|&idx| {
                    !self.image_cache.contains_key(&idx) &&
                        !currently_caching.contains(&idx) &&
                        Some(idx) != *loading
                })
                .collect()
        };

        if missing_indices.is_empty() {
            return;
        }

        println!("Scheduling caching from furthest viewed position {} for indices: {:?}",
                 cache_start_idx, missing_indices);

        let (tx, rx) = std::sync::mpsc::channel();
        self.cache_update_receiver = Some(rx);

        let images = self.images.clone();
        let ctx_clone = ctx;
        let currently_caching = self.currently_caching.clone();
        let loading_image = self.loading_image.clone();

        thread::spawn(move || {
            for idx in missing_indices {
                // Check if the main thread is loading this image
                {
                    let loading = loading_image.lock().unwrap();
                    if Some(idx) == *loading {
                        continue;
                    }
                }

                // Mark this index as being processed
                {
                    let mut caching_set = currently_caching.lock().unwrap();
                    if caching_set.contains(&idx) {
                        continue;
                    }
                    caching_set.insert(idx);
                }

                if let Some(image) = images.get(idx) {
                    let canonical_path = Self::get_canonical_path(&image.path);
                    println!("Starting to cache: {}", canonical_path.display());

                    if let Ok(img_reader) = ImageReader::open(&canonical_path) {
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
                            let texture = ctx_clone.load_texture(
                                format!("preloaded_image_{}", idx),
                                color_image,
                                egui::TextureOptions::default(),
                            );

                            // Send the update through the channel
                            let update = CacheUpdate { idx, texture };
                            if tx.send(update).is_ok() {
                                println!("Successfully cached {} in {:?}",
                                         canonical_path.display(), start.elapsed());
                            }
                        }
                    }
                }

                // Remove from currently_caching when done
                let mut caching_set = currently_caching.lock().unwrap();
                caching_set.remove(&idx);
            }
        });
    }




    fn backup_dataset(&mut self) {
        if let Some(dir) = &self.current_dir {
            let backup_dir = dir.join("backup");

            // Clear any locks on images
            self.current_texture = None;
            self.image_cache.clear();

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
                    return;
                }

                if let Err(err) = fs::remove_dir_all(&backup_dir) {
                    eprintln!("Failed to delete existing backup directory: {}", err);
                    self.feedback_message = Some(format!("Error: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    return;
                }
            }

            if let Err(err) = fs::create_dir_all(&backup_dir) {
                eprintln!("Failed to create backup directory: {}", err);
                self.feedback_message = Some(format!("Error during backup creation: {}", err));
                self.feedback_timer = Some(std::time::Instant::now());
                return;
            }

            for image in &self.images {
                let tags_path = image.path.with_extension("txt");

                if let Err(err) = fs::copy(&image.path, backup_dir.join(image.path.file_name().unwrap())) {
                    eprintln!("Failed to backup image {}: {}", image.path.display(), err);
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    return;
                }

                if let Err(err) = fs::copy(&tags_path, backup_dir.join(tags_path.file_name().unwrap())) {
                    eprintln!("Failed to backup tags {}: {}", tags_path.display(), err);
                    self.feedback_message = Some(format!("Error during backup: {}", err));
                    self.feedback_timer = Some(std::time::Instant::now());
                    return;
                }
            }

            self.feedback_message = Some("Backup completed successfully!".to_string());
            self.feedback_timer = Some(std::time::Instant::now());
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


    #[allow(dead_code)]
    fn save(&self, storage: &mut dyn eframe::Storage) {
        if let Ok(json) = serde_json::to_string(&self.images) {
            storage.set_string("image_tagger_data", json);
        }
    }


    fn load_directory(&mut self, path: &Path) {
        self.images.clear();
        self.image_cache.clear();
        *self.forward_cache_position.lock().unwrap() = None;
        *self.backward_cache_position.lock().unwrap() = None;

        // Load images as before
        for entry in WalkDir::new(path)
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
        {
            let path = entry.path().to_path_buf();
            let tags = self.load_tags_for_image(&path).unwrap_or_default();
            self.images.push(ImageData { path, tags });
        }

        println!("Starting background caching for {} images...", self.images.len());
        self.current_dir = Some(path.to_path_buf());
        self.current_image_idx = 0;

        // Start background caching
        self.start_background_caching(Arc::new(egui::Context::default()));
    }



    fn load_tags_for_image(&self, image_path: &Path) -> Result<Vec<String>, std::io::Error> {
        let tags_path = image_path.with_extension("txt"); // Use .txt extension for tag files
        if tags_path.exists() {
            let content = fs::read_to_string(tags_path)?;
            Ok(content
                .split(',')
                .map(|tag| tag.trim().to_string()) // Split tags by commas and trim spaces
                .filter(|tag| !tag.is_empty()) // Filter out empty tags
                .collect())
        } else {
            Ok(Vec::new()) // Return an empty list if the tag file does not exist
        }
    }


    fn save_tags_for_image(&self, image_data: &ImageData) -> Result<(), std::io::Error> {
        let tags_path = image_data.path.with_extension("txt"); // Use .txt extension for tag files
        fs::write(tags_path, image_data.tags.join(", ")) // Save tags as a comma-separated string
    }


    fn remove_duplicates(&mut self, image_data: &mut ImageData) {
        let mut seen = std::collections::HashSet::new();
        image_data.tags.retain(|tag| seen.insert(tag.clone()));
    }

    fn count_tag_occurrences(&self, search_tag: &str) -> usize {
        self.images
            .iter()
            .map(|img| img.tags.iter().filter(|t| *t == search_tag).count())
            .sum()
    }

    fn get_tag_frequencies(&self) -> Vec<(String, usize)> {
        let mut freq_map: HashMap<String, usize> = HashMap::new();

        for image in &self.images {
            for tag in &image.tags {
                *freq_map.entry(tag.clone()).or_insert(0) += 1;
            }
        }

        let mut freq_vec: Vec<_> = freq_map.into_iter().collect();
        if self.sort_by_frequency {
            freq_vec.sort_by(|a, b| {
                if self.sort_ascending {
                    a.1.cmp(&b.1)
                } else {
                    b.1.cmp(&a.1)
                }
            });
        } else {
            freq_vec.sort_by(|a, b| {
                if self.sort_ascending {
                    a.0.cmp(&b.0)
                } else {
                    b.0.cmp(&a.0)
                }
            });
        }

        freq_vec
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
            self.modified_files.insert(current_image.path.clone(), true); // Mark as modified
        }
    }





    fn handle_tag_removal_for_image(&mut self, tag: String) {
        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
            current_image.tags.retain(|t| t != &tag);

            // Save the updated tags
            let tags_path = current_image.path.with_extension("tags");
            fs::write(tags_path, current_image.tags.join("\n")).ok();
        }
    }
}


impl eframe::App for ImageTagger {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any pending cache updates
        if let Some(rx) = &self.cache_update_receiver {
            while let Ok(update) = rx.try_recv() {
                // Double check before inserting
                if !self.image_cache.contains_key(&update.idx) {
                    self.image_cache.insert(update.idx, update.texture);
                    println!("Cache size is now: {} images", self.image_cache.len());
                }
            }
        }

        if self.is_caching {
            let cached_count = *self.cached_images_count.lock().unwrap();
            self.cache_progress = cached_count as f32 / self.total_images_to_cache as f32;

            if cached_count >= self.total_images_to_cache {
                self.is_caching = false;
            }
        }

        if self.state_changed {
            if self.current_texture.is_none() {
                self.load_image_texture(ctx);
            }
            self.state_changed = false;
        }

        if self.current_texture.is_none() {
            self.load_image_texture(ctx);
        }

        // Log the currently displayed image only if it has changed
        if self.last_logged_image_idx != Some(self.current_image_idx) {
            if let Some(current_image) = self.images.get(self.current_image_idx) {
                let file_size = fs::metadata(&current_image.path).map(|m| m.len()).unwrap_or(0);
                println!(
                    "Currently displaying image: {} (Size: {} KB)",
                    current_image.path.display(),
                    file_size / 1024
                );
            }
            self.last_logged_image_idx = Some(self.current_image_idx);
        }


        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            if let Some(message) = &self.feedback_message {
                ui.colored_label(egui::Color32::GREEN, message);
            }

            // Clear feedback after 3 seconds
            if let Some(timer) = self.feedback_timer {
                if timer.elapsed().as_secs() >= 3 {
                    self.feedback_message = None;
                    self.feedback_timer = None;
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    self.save_all();
                }
                if ui.button("Backup").clicked() {
                    self.backup_dataset();
                }

                // Add activation tag input and button
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

            // Show caching progress bar
            if self.is_caching {
                ui.horizontal(|ui| {
                    ui.label("Caching progress:");
                    ui.add(egui::ProgressBar::new(self.cache_progress)
                        .show_percentage()
                        .desired_width(200.0));
                });
            }

            if let Some(message) = &self.feedback_message {
                ui.colored_label(egui::Color32::GREEN, message);
            }
        });



        // Left panel with image display
        egui::SidePanel::left("image_panel").show(ctx, |ui| {
            // Add directory picker at the top
            ui.horizontal(|ui| {
                if ui.button("Open Directory").clicked() {
                    if let Some(path) = FileDialog::new().pick_folder() {
                        self.load_directory(&path);
                    }
                }
            });

            // Display current directory
            if let Some(dir) = &self.current_dir {
                ui.label(format!("Current directory: {}", dir.display()));
            }

            // Add separator
            ui.separator();

            // Navigation buttons moved above the image
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
                ui.separator(); // Add another separator for visual clarity
            }

            // Image display
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

                // Buttons for sorting
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

                // Display tags
                egui::ScrollArea::vertical()
                    .id_source(format!("tag_display_{}", self.current_image_idx))
                    .show(ui, |ui| {
                        for tag in &self.images[self.current_image_idx].tags {

                            ui.label(tag); // Display each tag
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

                ui.add_space(5.0); // Add spacing between buttons
                ui.horizontal(|ui| {
                    if ui.button("Remove Duplicates (Current)").clicked() {
                        if let Some(current_image) = self.images.get_mut(self.current_image_idx) {
                            let mut seen = std::collections::HashSet::new();
                            current_image.tags.retain(|tag| seen.insert(tag.clone()));
                            self.modified_files.insert(current_image.path.clone(), true); // Mark as modified
                        }
                    }
                    if ui.button("Remove Duplicates (All)").clicked() {
                        self.remove_duplicates_for_all(); // Call the updated method
                    }
                });





                // Tag input
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

                // Display and manage existing tags
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

                // Handle tag actions
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
        Box::new(|cc| Box::new(ImageTagger::new(cc))),
    )
}
