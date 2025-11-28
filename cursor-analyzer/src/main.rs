use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::Instant;
use crossbeam_channel::{unbounded, Receiver, Sender};
use image::RgbaImage;
use std::thread;
use std::process::{Command, Stdio, Child, ChildStdout};
use std::io::{Read, BufReader};
use regex::Regex;
use ffmpeg_sidecar::download::auto_download;

#[derive(Debug, Clone)]
enum AppCommand {
    LoadFile(PathBuf),
    Seek(f64), 
    Step,      
    Play,      
    Pause,     
}

#[derive(Debug)]
enum AppEvent {
    FrameReady {
        image: RgbaImage,
        width: u32,
        height: u32,
        position: Option<[f32; 2]>,
    },
    Metadata {
        duration: f64,
        width: u32,
        height: u32,
    },
    Error(String),
}

struct VideoApp {

    file_path: Option<PathBuf>,
    speed: f64,
    interval_ms: u64,
    is_simulating: bool,
    last_sim_time: Instant,

    is_playing: bool,
    last_play_frame: Instant,

    texture: Option<egui::TextureHandle>,
    current_frame_size: [u32; 2],
    video_duration: f64,
    current_time: f64, 

    positions: Vec<[f32; 2]>,

    cmd_tx: Sender<AppCommand>,
    event_rx: Receiver<AppEvent>,
}

impl VideoApp {
    fn new() -> Self {
        let (cmd_tx, cmd_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();

        if let Err(e) = auto_download() {
            eprintln!("Failed to download ffmpeg: {}", e);
        }

        thread::spawn(move || {
            video_worker(cmd_rx, event_tx);
        });

        Self {
            file_path: None,
            speed: 1.0,
            interval_ms: 1000,
            is_simulating: false,
            last_sim_time: Instant::now(),
            is_playing: false,
            last_play_frame: Instant::now(),
            texture: None,
            current_frame_size: [0, 0],
            video_duration: 0.0,
            current_time: 0.0,
            positions: Vec::new(),
            cmd_tx,
            event_rx,
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::FrameReady { image, width, height, position } => {
                    self.current_frame_size = [width, height];

                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        image.as_flat_samples().as_slice(),
                    );

                    self.texture = Some(ctx.load_texture(
                        "video_frame",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));

                    if let Some(pos) = position {
                        self.positions.push(pos);
                    }

                    self.current_time += 1.0 / 60.0;
                }
                AppEvent::Metadata { duration, width, height } => {
                    self.video_duration = duration;
                    self.current_frame_size = [width, height];
                    self.current_time = 0.0;
                }
                AppEvent::Error(msg) => {
                    eprintln!("Video Error: {}", msg);
                }
            }
        }
    }
}

impl eframe::App for VideoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events(ctx);

        if self.is_simulating {
            if self.last_sim_time.elapsed().as_millis() as u64 >= self.interval_ms {
                let _ = self.cmd_tx.send(AppCommand::Step);
                self.last_sim_time = Instant::now();
            }
            ctx.request_repaint();
        }

        if self.is_playing && !self.is_simulating {
             let target_dt = 1.0 / (60.0 * self.speed);
             if self.last_play_frame.elapsed().as_secs_f64() >= target_dt {
                 let _ = self.cmd_tx.send(AppCommand::Step);
                 self.last_play_frame = Instant::now();
             }
             ctx.request_repaint();
        }

        egui::TopBottomPanel::bottom("controls").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open File").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Video", &["mp4"]).pick_file() {
                        self.file_path = Some(path.clone());
                        self.positions.clear();
                        self.is_playing = false;
                        let _ = self.cmd_tx.send(AppCommand::LoadFile(path));
                    }
                }

                if ui.button(if self.is_playing { "Pause" } else { "Play" }).clicked() {
                    self.is_playing = !self.is_playing;
                    self.last_play_frame = Instant::now();
                    if self.is_playing && self.is_simulating {
                        self.is_simulating = false; 
                    }
                }

                ui.label("Speed:");
                ui.add(egui::Slider::new(&mut self.speed, 0.07..=2.0).step_by(0.01));

                ui.label("Interval (ms):");
                ui.add(egui::DragValue::new(&mut self.interval_ms).speed(10).range(1..=10000));

                if ui.button(if self.is_simulating { "Stop Magic" } else { "Magic" }).clicked() {
                    self.is_simulating = !self.is_simulating;
                    if self.is_simulating {
                        self.is_playing = false; 
                        self.last_sim_time = Instant::now();
                    }
                }

                if ui.button("Clear Pos").clicked() {
                    self.positions.clear();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let available_size = ui.available_size();

            if let Some(tex) = &self.texture {
                 let tex_size = tex.size_vec2();

                 let scale_x = available_size.x / tex_size.x;
                 let scale_y = available_size.y / tex_size.y;
                 let scale = scale_x.min(scale_y);

                 let display_size = tex_size * scale;

                 let (rect, _response) = ui.allocate_exact_size(display_size, egui::Sense::click());

                 ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                 );

                 if !self.positions.is_empty() {

                     let scale_factor = display_size.x / tex_size.x;

                     let points: Vec<egui::Pos2> = self.positions.iter().map(|p| {
                         rect.min + egui::vec2(p[0] * scale_factor, p[1] * scale_factor)
                     }).collect();

                     for p in &points {
                         ui.painter().circle_filled(*p, 5.0 * scale_factor, egui::Color32::RED);
                     }

                     if points.len() > 1 {
                        ui.painter().add(egui::Shape::line(
                            points,
                            egui::Stroke::new(3.0 * scale_factor, egui::Color32::RED),
                        ));
                     }
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("Load a video file...");
                });
            }
        });
    }
}

struct VideoWorker {
    rx: Receiver<AppCommand>,
    tx: Sender<AppEvent>,
    current_process: Option<Child>,
    current_reader: Option<BufReader<ChildStdout>>,
    current_file: Option<PathBuf>,
    width: u32,
    height: u32,
    duration: f64,
}

impl VideoWorker {
    fn new(rx: Receiver<AppCommand>, tx: Sender<AppEvent>) -> Self {
        Self {
            rx,
            tx,
            current_process: None,
            current_reader: None,
            current_file: None,
            width: 0,
            height: 0,
            duration: 0.0,
        }
    }

    fn run(&mut self) {
        loop {
            match self.rx.recv() {
                Ok(cmd) => match cmd {
                    AppCommand::LoadFile(path) => {
                        self.load_file(path);
                    },
                    AppCommand::Step => {
                        self.read_next_frame();
                    },
                    AppCommand::Seek(t) => {
                        self.seek(t);
                    },
                    AppCommand::Play => {},
                    AppCommand::Pause => {},
                },
                Err(_) => break,
            }
        }
    }

    fn load_file(&mut self, path: PathBuf) {

        match probe_file(&path) {
            Ok((dur, w, h)) => {
                self.duration = dur;
                self.width = w;
                self.height = h;
                self.current_file = Some(path.clone());

                let _ = self.tx.send(AppEvent::Metadata {
                    duration: dur,
                    width: w,
                    height: h,
                });

                self.start_ffmpeg(0.0);

                self.read_next_frame();
            },
            Err(e) => {
                let _ = self.tx.send(AppEvent::Error(e));
            }
        }
    }

    fn start_ffmpeg(&mut self, start_time: f64) {
        if let Some(mut child) = self.current_process.take() {
             let _ = child.kill();
             let _ = child.wait();
        }
        self.current_reader = None;

        if let Some(path) = &self.current_file {
            let binary = if cfg!(windows) { "ffmpeg" } else { "./ffmpeg" };
            let mut cmd = Command::new(binary);
            cmd.arg("-i").arg(path.to_str().unwrap());

            if start_time > 0.0 {
                cmd.arg("-ss").arg(&format!("{}", start_time));
            }

            cmd.args(&[
                "-f", "image2pipe",
                "-pix_fmt", "rgba",
                "-vcodec", "rawvideo",
                "-"
            ]);
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::null());

            match cmd.spawn() {
                Ok(mut child) => {
                    if let Some(stdout) = child.stdout.take() {
                        self.current_reader = Some(BufReader::new(stdout));
                        self.current_process = Some(child);
                    }
                },
                Err(e) => {
                     let _ = self.tx.send(AppEvent::Error(format!("FFmpeg spawn error: {}", e)));
                }
            }
        }
    }

    fn seek(&mut self, time: f64) {
        self.start_ffmpeg(time);
        self.read_next_frame();
    }

    fn read_next_frame(&mut self) {
        if self.width == 0 || self.height == 0 { return; }

        if let Some(reader) = &mut self.current_reader {
            let frame_size = (self.width * self.height * 4) as usize;
            let mut buffer = vec![0u8; frame_size];

            match reader.read_exact(&mut buffer) {
                Ok(_) => {

                     let pos = find_position(&buffer, self.width as usize, self.height as usize);

                     if let Some(img) = RgbaImage::from_raw(self.width, self.height, buffer) {
                         let _ = self.tx.send(AppEvent::FrameReady {
                             image: img,
                             width: self.width,
                             height: self.height,
                             position: pos,
                         });
                     }
                },
                Err(_e) => {

                }
            }
        }
    }
}

fn probe_file(path: &Path) -> Result<(f64, u32, u32), String> {
    let binary = if cfg!(windows) { "ffmpeg" } else { "./ffmpeg" };
    let output = Command::new(binary)
        .arg("-i")
        .arg(path.to_str().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|c| c.wait_with_output())
        .map_err(|e| e.to_string())?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    let dur_regex = Regex::new(r"Duration: (\d{2}):(\d{2}):(\d{2}\.\d+)").unwrap();
    let mut duration = 0.0;
    if let Some(caps) = dur_regex.captures(&stderr) {
        let h: f64 = caps[1].parse().unwrap_or(0.0);
        let m: f64 = caps[2].parse().unwrap_or(0.0);
        let s: f64 = caps[3].parse().unwrap_or(0.0);
        duration = h * 3600.0 + m * 60.0 + s;
    }

    let res_regex = Regex::new(r"Video:.* (\d{3,})x(\d{3,})").unwrap();
    let mut width = 0;
    let mut height = 0;
    if let Some(caps) = res_regex.captures(&stderr) {
        width = caps[1].parse().unwrap_or(0);
        height = caps[2].parse().unwrap_or(0);
    }

    if width > 0 && height > 0 {
        Ok((duration, width, height))
    } else {
        Err("Could not parse video metadata".to_string())
    }
}

fn video_worker(rx: Receiver<AppCommand>, tx: Sender<AppEvent>) {
    let mut worker = VideoWorker::new(rx, tx);
    worker.run();
}

fn find_position(data: &[u8], width: usize, _height: usize) -> Option<[f32; 2]> {
    let px_for_row = width * 4;
    let px_for_col = 4;
    let lim_max = 210;
    let lim_min = 90;

    let limit = data.len().saturating_sub(20 * px_for_row);

    for i in (0..limit).step_by(4) {
        if i + 2 >= data.len() { continue; }

        if data[i] >= lim_max && data[i+1] >= lim_max && data[i+2] >= lim_max {

            if i + px_for_col + 2 >= data.len() { continue; }

            if data[i + px_for_col] >= lim_min
               || data[i + px_for_col + 1] >= lim_min
               || data[i + px_for_col + 2] >= lim_min {
                continue;
            }

            let mut valid_vertical = true;
            for j in 1..14 {
                 let idx = i + j * px_for_row;
                 if idx + 2 >= data.len() { valid_vertical = false; break; }

                 if data[idx] <= lim_max
                    || data[idx+1] <= lim_max
                    || data[idx+2] <= lim_max {
                     valid_vertical = false;
                     break;
                 }
            }
            if !valid_vertical { continue; }

            let mut valid_left = true;
            for j in 0..14 {
                let base = i + j * px_for_row;
                if base < px_for_col { valid_left = false; break; }
                let idx = base - px_for_col;

                if idx + 2 >= data.len() { valid_left = false; break; }

                if data[idx] >= lim_min
                   || data[idx+1] >= lim_min
                   || data[idx+2] >= lim_min {
                    valid_left = false;
                    break;
                }
            }
            if !valid_left { continue; }

            let x = i % px_for_row;
            let y = (i - x) / px_for_row;

            return Some([(x / 4) as f32, y as f32]);
        }
    }
    None
}

fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1300.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Cursor analyser",
        options,
        Box::new(|_cc| Ok(Box::new(VideoApp::new()))),
    )
}