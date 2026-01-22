use std::process::Command;
use std::{
    env,
    io::{stdout, Write},
    marker::PhantomData,
    path::Path,
    sync::mpsc::channel,
    thread::{sleep, spawn},
    time::{Duration, Instant},
};
use toy_arms::external::{read, Process};
use winapi::um::winnt::HANDLE;
use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
use winapi::um::processenv::GetStdHandle;
use winapi::um::winbase::STD_INPUT_HANDLE;

mod offsets;
use offsets::{Pointer, RekordboxOffsets};

mod osc;
use osc::OscSender;

use serde_json;

extern "C" {
    fn _getch() -> core::ffi::c_char;
}

fn getch() -> i8 {
    unsafe { _getch() }
}

/// Отключает QuickEdit Mode в Windows консоли
/// Это предотвращает "зависание" при клике в консоль
fn disable_quick_edit() {
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        if handle.is_null() {
            return;
        }
        
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            // ENABLE_QUICK_EDIT_MODE = 0x0040
            // ENABLE_EXTENDED_FLAGS = 0x0080
            let new_mode = (mode & !0x0040) | 0x0080;
            SetConsoleMode(handle, new_mode);
        }
    }
}

struct Value<T> {
    address: usize,
    handle: HANDLE,
    _marker: PhantomData<T>,
}

impl<T> Value<T> {
    fn new(h: HANDLE, base: usize, offsets: Pointer) -> Option<Value<T>> {
        let mut address = base;

        for offset in offsets.offsets {
            address = match read::<usize>(h, address + offset) {
                Ok(addr) => addr,
                Err(_) => return None,
            };
        }
        address += offsets.final_offset;

        Some(Value::<T> {
            address,
            handle: h,
            _marker: PhantomData::<T>,
        })
    }

    fn read(&self) -> Option<T> {
        read::<T>(self.handle, self.address).ok()
    }

    fn read_bytes(&self, times: usize) -> Option<Vec<u8>> {
        let mut byte_vec = Vec::new();
        for _t in 0..times {
            let read_mem_bytes = read::<u8>(self.handle, self.address + (_t)).ok()?;
            byte_vec.push(read_mem_bytes);
        }
        Some(byte_vec)
    }
}

pub struct Rekordbox {
    handle: HANDLE,
    base: usize,
    offsets: RekordboxOffsets,
    master_bpm_val: Option<Value<f32>>,
    bar1_val: Option<Value<i32>>,
    beat1_val: Option<Value<i32>>,
    bar2_val: Option<Value<i32>>,
    beat2_val: Option<Value<i32>>,
    masterdeck_index_val: Option<Value<u8>>,

    deck1_time_val: Option<Value<i32>>,
    deck2_time_val: Option<Value<i32>>,
    deck1_track_id_val: Option<Value<i32>>,
    deck2_track_id_val: Option<Value<i32>>,
    api_bearer_val: Option<Value<Vec<u8>>>,

    pub beats1: i32,
    pub beats2: i32,
    pub master_beats: i32,
    pub master_bpm: f32,
    pub masterdeck_index: u8,
    pub deck1_time: i32,
    pub deck2_time: i32,
    pub deck1_track_id: i32,
    pub deck2_track_id: i32,
    pub master_time: i32,
    pub api_bearer: String,
}

impl Rekordbox {
    fn new(offsets: RekordboxOffsets) -> Result<Self, String> {
        let rb = match Process::from_process_name("rekordbox.exe") {
            Ok(p) => p,
            Err(e) => {
                return Err(format!(
                    "Could not find Rekordbox process!\n\
                     Make sure Rekordbox is running.\n\
                     If running, try launching rkbx_osc as Administrator.\n\
                     Error: {:?}", e
                ));
            }
        };
        let h = rb.process_handle;

        let base = match rb.get_module_base("rekordbox.exe") {
            Ok(b) => b,
            Err(e) => return Err(format!("Could not get Rekordbox module base address: {:?}", e)),
        };

        let master_bpm_val = Value::new(h, base, offsets.master_bpm.clone());
        let api_bearer_val = Value::new(h, base, offsets.api_bearer.clone());
        let bar1_val = Value::new(h, base, offsets.deck1bar.clone());
        let beat1_val = Value::new(h, base, offsets.deck1beat.clone());
        let bar2_val = Value::new(h, base, offsets.deck2bar.clone());
        let beat2_val = Value::new(h, base, offsets.deck2beat.clone());
        let deck1_track_id_val = Value::new(h, base, offsets.deck1_track_id.clone());
        let deck1_time_val = Value::new(h, base, offsets.deck1_time.clone());
        let deck2_track_id_val = Value::new(h, base, offsets.deck2_track_id.clone());
        let deck2_time_val = Value::new(h, base, offsets.deck2_time.clone());
        let masterdeck_index_val = Value::new(h, base, offsets.masterdeck_index.clone());

        Ok(Self {
            handle: h,
            base,
            offsets,
            master_bpm_val,
            bar1_val,
            beat1_val,
            bar2_val,
            beat2_val,

            deck1_time_val,
            deck2_time_val,

            deck1_track_id_val,
            deck2_track_id_val,
            api_bearer_val,

            masterdeck_index_val,

            beats1: -1,
            beats2: -1,
            master_bpm: 120.0,
            masterdeck_index: 255,
            master_beats: 0,
            master_time: 0,
            deck1_track_id: 0,
            deck2_track_id: 0,
            deck1_time: 0,
            deck2_time: 0,
            api_bearer: "".to_string(),
        })
    }

    fn update(&mut self) {
        // Retry initializing failed values
        if self.master_bpm_val.is_none() {
            self.master_bpm_val = Value::new(self.handle, self.base, self.offsets.master_bpm.clone());
        }
        if self.bar1_val.is_none() {
            self.bar1_val = Value::new(self.handle, self.base, self.offsets.deck1bar.clone());
        }
        if self.beat1_val.is_none() {
            self.beat1_val = Value::new(self.handle, self.base, self.offsets.deck1beat.clone());
        }
        if self.bar2_val.is_none() {
            self.bar2_val = Value::new(self.handle, self.base, self.offsets.deck2bar.clone());
        }
        if self.beat2_val.is_none() {
            self.beat2_val = Value::new(self.handle, self.base, self.offsets.deck2beat.clone());
        }
        if self.masterdeck_index_val.is_none() {
            self.masterdeck_index_val = Value::new(self.handle, self.base, self.offsets.masterdeck_index.clone());
        }
        if self.deck1_track_id_val.is_none() {
            self.deck1_track_id_val = Value::new(self.handle, self.base, self.offsets.deck1_track_id.clone());
        }
        if self.deck2_track_id_val.is_none() {
            self.deck2_track_id_val = Value::new(self.handle, self.base, self.offsets.deck2_track_id.clone());
        }
        if self.deck1_time_val.is_none() {
            self.deck1_time_val = Value::new(self.handle, self.base, self.offsets.deck1_time.clone());
        }
        if self.deck2_time_val.is_none() {
            self.deck2_time_val = Value::new(self.handle, self.base, self.offsets.deck2_time.clone());
        }

        // Read values
        self.master_bpm = self.master_bpm_val.as_ref().and_then(|v| v.read()).unwrap_or(120.0);
        
        if let (Some(bar1), Some(beat1)) = (&self.bar1_val, &self.beat1_val) {
            if let (Some(b1), Some(bt1)) = (bar1.read(), beat1.read()) {
                self.beats1 = b1 * 4 + bt1;
            }
        }
        if let (Some(bar2), Some(beat2)) = (&self.bar2_val, &self.beat2_val) {
            if let (Some(b2), Some(bt2)) = (bar2.read(), beat2.read()) {
                self.beats2 = b2 * 4 + bt2;
            }
        }
        
        self.masterdeck_index = self.masterdeck_index_val.as_ref().and_then(|v| v.read()).unwrap_or(255);
        self.deck1_track_id = self.deck1_track_id_val.as_ref().and_then(|v| v.read()).unwrap_or(0);
        self.deck2_track_id = self.deck2_track_id_val.as_ref().and_then(|v| v.read()).unwrap_or(0);
        self.deck1_time = self.deck1_time_val.as_ref().and_then(|v| v.read()).unwrap_or(0);
        self.deck2_time = self.deck2_time_val.as_ref().and_then(|v| v.read()).unwrap_or(0);

        if self.masterdeck_index == 0 {
            self.master_beats = self.beats1;
            self.master_time = self.deck1_time;
        } else if self.masterdeck_index == 1 {
            self.master_beats = self.beats2;
            self.master_time = self.deck2_time;
        }
    }

    pub fn update_api_bearer(&mut self) {
        if self.api_bearer_val.is_none() {
            self.api_bearer_val = Value::new(self.handle, self.base, self.offsets.api_bearer.clone());
        }
        
        if let Some(ref api_val) = self.api_bearer_val {
            if let Some(api_bearer_vec) = api_val.read_bytes(64) {
                // Обрезаем по первому null-байту
                let null_pos = api_bearer_vec.iter().position(|&b| b == 0).unwrap_or(api_bearer_vec.len());
                let clean_vec = &api_bearer_vec[..null_pos];
                
                self.api_bearer = match std::str::from_utf8(clean_vec) {
                    Ok(v) => v.trim().to_string(),
                    Err(_) => String::new(),
                };
            }
        }
    }
}

pub struct BeatKeeper {
    rb: Option<Rekordbox>,
    last_beat: i32,
    last_time: i32,

    pub api_bearer: String,
    
    pub last_d1track: i32,
    pub last_d2track: i32,
    pub last_master_track: i32,
    pub last_master_path: String,
    pub last_master_title: String,
    pub last_master_artist: String,

    pub beat_fraction: f32,
    pub last_masterdeck_index: u8,
    pub offset_micros: f32,
    pub last_bpm: f32,
    pub new_beat: bool,
    pub new_track: bool,
    pub new_time: bool,
}

impl BeatKeeper {
    pub fn new(offsets: RekordboxOffsets) -> Result<Self, String> {
        let rb = Rekordbox::new(offsets)?;
        Ok(BeatKeeper {
            rb: Some(rb),
            last_beat: 0,
            last_time: 0,
            last_d1track: 0,
            last_d2track: 0,
            last_master_track: 0,
            last_master_path: "".to_string(),
            last_master_title: "".to_string(),
            last_master_artist: "".to_string(),
            api_bearer: "".to_string(),
            beat_fraction: 1.,
            last_masterdeck_index: 0,
            offset_micros: 0.,
            last_bpm: 0.,
            new_beat: false,
            new_track: false,
            new_time: false,
        })
    }

    pub fn update(&mut self, delta: Duration) {
        if let Some(rb) = &mut self.rb {
            let beats_per_micro = rb.master_bpm / 60. / 1000000.;
            let mut master_track_changed = false;

            rb.update();

            if rb.masterdeck_index != self.last_masterdeck_index && rb.masterdeck_index <= 1 {
                self.last_masterdeck_index = rb.masterdeck_index;
                self.last_beat = rb.master_beats;
                if rb.masterdeck_index == 0 {
                    self.last_master_track = rb.deck1_track_id;
                } else {
                    self.last_master_track = rb.deck2_track_id;
                }
                master_track_changed = true;
            }

            if rb.deck1_track_id != self.last_d1track && rb.deck1_track_id > 0 {
                println!("[BeatKeeper] Deck 1 track change: {}", rb.deck1_track_id);
                self.last_d1track = rb.deck1_track_id;
                if rb.masterdeck_index == 0 {
                    self.last_master_track = rb.deck1_track_id;
                    master_track_changed = true;
                }
            }

            if rb.deck2_track_id != self.last_d2track && rb.deck2_track_id > 0 {
                println!("[BeatKeeper] Deck 2 track change: {}", rb.deck2_track_id);
                self.last_d2track = rb.deck2_track_id;
                if rb.masterdeck_index == 1 {
                    self.last_master_track = rb.deck2_track_id;
                    master_track_changed = true;
                }
            }

            if (rb.master_beats - self.last_beat).abs() > 0 {
                self.last_beat = rb.master_beats;
                self.beat_fraction = 0.;
                self.new_beat = true;
            }

            if rb.master_time != self.last_time {
                self.last_time = rb.master_time;
                self.new_time = true;
            }
            
            if master_track_changed {
                if self.last_master_track > 0 {
                    println!("[Track] Master track changed to ID: {}", self.last_master_track);
                    let res = get_track_info(self.last_master_track, &self.api_bearer);
                    
                    // Проверяем что ответ успешный (нет поля "code" или code == 200)
                    let is_error = res.get("code").map(|c| c.as_i64().unwrap_or(200) != 200).unwrap_or(false);
                    let is_404 = res.get("code").map(|c| c.as_i64().unwrap_or(0) == 404).unwrap_or(false);
                    
                    if !is_error && !is_404 && res.get("item").is_some() {
                        self.last_master_path = res["item"]["FolderPath"].as_str().unwrap_or("").to_string();
                        self.last_master_title = res["item"]["Title"].as_str().unwrap_or("").to_string();
                        self.last_master_artist = res["item"]["ArtistName"].as_str().unwrap_or("").to_string();
                        
                        // Fallback to filename if title is empty
                        if self.last_master_title.is_empty() {
                            self.last_master_title = res["item"]["FileNameL"].as_str().unwrap_or("").to_string();
                        }
                        
                        println!("[Track] Parsed: {} - {}", self.last_master_artist, self.last_master_title);
                        self.new_track = true;
                    } else {
                        println!("[Track] API returned error or no item. is_error={}, is_404={}, has_item={}", 
                            is_error, is_404, res.get("item").is_some());
                    }
                }
            }

            self.beat_fraction =
                (self.beat_fraction + delta.as_micros() as f32 * beats_per_micro) % 1.;
        } else {
            self.beat_fraction = (self.beat_fraction + delta.as_secs_f32() * 130. / 60.) % 1.;
        }
    }

    pub fn update_api_bearer(&mut self) {
        if let Some(rb) = &mut self.rb {
            rb.update_api_bearer();
            self.api_bearer = rb.api_bearer.clone();
        }
    }

    pub fn get_bpm_changed(&mut self) -> Option<f32> {
        if let Some(rb) = &self.rb {
            if rb.master_bpm != self.last_bpm {
                self.last_bpm = rb.master_bpm;
                return Some(rb.master_bpm);
            }
        }
        None
    }

    pub fn get_new_beat(&mut self) -> bool {
        if self.new_beat {
            self.new_beat = false;
            return true;
        }
        false
    }

    pub fn get_new_time(&mut self) -> bool {
        if self.new_time {
            self.new_time = false;
            return true;
        }
        false
    }

    pub fn get_new_master_track(&mut self) -> bool {
        if self.new_track {
            self.new_track = false;
            return true;
        }
        false
    }
}

const CHARS: [&str; 4] = ["|", "/", "-", "\\"];

/// Получение информации о треке через Rekordbox API
pub fn get_track_info(track_id: i32, api_key: &String) -> serde_json::Value {
    println!("[API] Fetching track info for ID: {}", track_id);
    println!("[API] Bearer token (first 20 chars): {}...", &api_key.chars().take(20).collect::<String>());
    
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());

    let url = format!("http://127.0.0.1:30001/api/v1/data/djmdContents/{}/", track_id);
    println!("[API] URL: {}", url);

    let response = client
        .get(&url)
        .header("User-Agent", "rekordbox/7.2.8.0001 Windows 11(64bit)")
        .header("Accept", "*/*")
        .header("Authorization", format!("Bearer {}", api_key))
        .send();

    match response {
        Ok(resp) => {
            let status = resp.status();
            println!("[API] Response status: {}", status);
            
            match resp.text() {
                Ok(text) => {
                    println!("[API] Response length: {} bytes", text.len());
                    if text.len() < 500 {
                        println!("[API] Response body: {}", text);
                    } else {
                        println!("[API] Response body (truncated): {}...", &text[..200]);
                    }
                    
                    match serde_json::from_str(&text) {
                        Ok(json) => json,
                        Err(e) => {
                            println!("[API] JSON parse error: {}", e);
                            serde_json::json!({"code": 500, "error": "parse_error"})
                        }
                    }
                },
                Err(e) => {
                    println!("[API] Failed to read response: {}", e);
                    serde_json::json!({"code": 500, "error": "read_error"})
                }
            }
        },
        Err(e) => {
            println!("[API] Request failed: {}", e);
            serde_json::json!({"code": 500, "error": "request_failed"})
        }
    }
}

fn main() {
    // Отключаем QuickEdit Mode чтобы консоль не зависала при клике
    disable_quick_edit();
    
    println!("===========================================");
    println!("  rkbx_osc v{} - Rekordbox to OSC Bridge", env!("CARGO_PKG_VERSION"));
    println!("  For RekordKaraoke lyrics sync");
    println!("===========================================");
    println!();

    if !Path::new("./offsets").exists() {
        println!("[Init] Offsets not found, downloading...");
        download_offsets();
    }

    let (tx, rx) = channel::<i8>();
    spawn(move || loop {
        tx.send(getch()).unwrap();
    });

    let args: Vec<String> = env::args().collect();

    let version_offsets = RekordboxOffsets::from_file("offsets");
    let mut versions: Vec<String> = version_offsets.keys().map(|x| x.to_string()).collect();
    versions.sort();
    versions.reverse();
    let mut target_version = versions[0].clone();
    let mut poll_rate: u64 = 60;
    
    // OSC настройки (по умолчанию для RekordKaraoke)
    let mut osc_source = "127.0.0.1:4450".to_string();
    let mut osc_dest = "127.0.0.1:4460".to_string();

    let mut args_iter = args.iter();
    args_iter.next();
    while let Some(arg) = args_iter.next() {
        let mut chars = arg.chars();
        if let Some(char) = chars.next() {
            if char == '-' {
                if let Some(flag) = chars.next() {
                    match flag.to_string().as_str() {
                        "u" => {
                            println!("Updating offsets...");
                            download_offsets();
                            return;
                        }
                        "p" => {
                            if let Some(poll_arg) = args_iter.next() {
                                match poll_arg.parse::<u64>() {
                                    Ok(value) => poll_rate = value,
                                    Err(_) => println!("Invalid poll_rate, using default: {}", poll_rate),
                                }
                            }
                        }
                        "v" => {
                            target_version = args_iter.next().unwrap().to_string();
                        }
                        "o" => {
                            // OSC destination в формате host:port
                            if let Some(dest) = args_iter.next() {
                                osc_dest = dest.to_string();
                            }
                        }
                        "s" => {
                            // OSC source в формате host:port
                            if let Some(src) = args_iter.next() {
                                osc_source = src.to_string();
                            }
                        }
                        "h" => {
                            println!(
                                "rkbx_osc v{} - Rekordbox to OSC Bridge for RekordKaraoke

USAGE:
  rkbx_osc.exe [flags]

FLAGS:
  -h         Print this help
  -u         Fetch latest offset list from GitHub and exit
  -v <ver>   Rekordbox version to target (default: {})
  -p <rate>  Poll rate in Hz (default: 60)
  -o <addr>  OSC destination address (default: 127.0.0.1:4460)
  -s <addr>  OSC source address (default: 127.0.0.1:4450)

CONTROLS:
  c          Quit
  r          Resend current track info

OSC MESSAGES SENT:
  /time/master          (float)  Current position in seconds
  /bpm/master/current   (float)  Current BPM
  /beat/master          (int)    Current beat number
  /track/master/title   (string) Track title
  /track/master/artist  (string) Track artist
  /track/master/path    (string) Track file path
  /deck/master          (int)    Active deck (0 or 1)

SUPPORTED REKORDBOX VERSIONS:
  {}
",
                                env!("CARGO_PKG_VERSION"),
                                versions[0],
                                versions.join(", ")
                            );
                            return;
                        }
                        c => {
                            println!("Unknown flag -{c}");
                        }
                    }
                }
            }
        }
    }

    let offsets = if let Some(offsets) = version_offsets.get(target_version.as_str()) {
        offsets
    } else {
        println!("[Error] Unsupported Rekordbox version: {target_version}");
        println!("[Error] Available versions: {}", versions.join(", "));
        return;
    };
    
    println!("[Config] Rekordbox version: {}", target_version);
    println!("[Config] Poll rate: {} Hz", poll_rate);

    // Инициализация OSC
    let osc = match OscSender::new(&osc_source, &osc_dest) {
        Ok(sender) => sender,
        Err(e) => {
            println!("[Error] Failed to initialize OSC: {}", e);
            return;
        }
    };

    println!();
    println!("Press 'c' to quit, 'r' to resend track info");
    println!();

    let mut keeper = match BeatKeeper::new(offsets.clone()) {
        Ok(k) => k,
        Err(e) => {
            println!("[Error] {}", e);
            println!();
            println!("Press any key to exit...");
            getch();
            return;
        }
    };

    let period = Duration::from_micros(1000000 / poll_rate);
    let mut last_instant = Instant::now();

    let mut count = 0;
    let mut step = 0;
    let mut stdout = stdout();

    // Get API bearer key
    keeper.update_api_bearer();
    if keeper.api_bearer.is_empty() {
        println!("[Warning] Could not get API bearer token");
        println!("[Warning] Track info will not be available!");
    } else {
        println!("[Init] API bearer acquired: {}...", &keeper.api_bearer.chars().take(20).collect::<String>());
        println!("[Init] Bearer length: {} chars", keeper.api_bearer.len());
    }

    println!("[Init] Entering main loop...");
    println!();
    
    loop {
        let delta = Instant::now() - last_instant;
        last_instant = Instant::now();

        // Периодически обновляем bearer token (каждые ~5 секунд)
        if count % (poll_rate as u32 * 5) == 0 {
            keeper.update_api_bearer();
        }

        keeper.update(delta);

        // Отправляем время при каждом изменении (главное для караоке синхронизации)
        if keeper.get_new_time() {
            osc.send_time(keeper.last_time);
        }

        // Отправляем beat
        if keeper.get_new_beat() {
            osc.send_beat(keeper.last_beat);
            
            // При смене трека отправляем информацию
            if keeper.get_new_master_track() {
                println!();
                println!("[OSC] Sending track: {} - {}", keeper.last_master_artist, keeper.last_master_title);
                osc.send_track_info(
                    &keeper.last_master_title,
                    &keeper.last_master_artist,
                    &keeper.last_master_path
                );
                osc.send_master_deck(keeper.last_masterdeck_index);
            }
        }

        // Отправляем BPM при изменении
        if let Some(bpm) = keeper.get_bpm_changed() {
            osc.send_bpm(bpm);
        }

        // Обработка клавиш
        while let Ok(key) = rx.try_recv() {
            match key {
                99 => { // 'c'
                    println!();
                    println!("[Exit] Goodbye!");
                    return;
                }
                114 => { // 'r'
                    println!();
                    println!("[Resend] {} - {}", keeper.last_master_artist, keeper.last_master_title);
                    osc.send_track_info(
                        &keeper.last_master_title,
                        &keeper.last_master_artist,
                        &keeper.last_master_path
                    );
                }
                _ => (),
            }
        }

        // Обновление статуса в консоли
        if count % 20 == 0 {
            step = (step + 1) % 4;

            print!(
                "\r{} [{:02}:{:02}] Deck {} | {:.1} BPM | {} - {}",
                CHARS[step],
                (keeper.last_time / 1000) / 60,
                (keeper.last_time / 1000) % 60,
                keeper.last_masterdeck_index + 1,
                keeper.last_bpm,
                keeper.last_master_artist,
                keeper.last_master_title
            );
            
            // Обрезаем длинные строки
            print!("                    ");
            stdout.flush().unwrap();
        }
        count = (count + 1) % 120;

        sleep(period);
    }
}

fn download_offsets() {
    match Command::new("curl")
        .args([
            "-o",
            "offsets",
            "https://raw.githubusercontent.com/fjel/rkbx_os2l/master/offsets",
        ])
        .output()
    {
        Ok(output) => {
            println!("{}", String::from_utf8(output.stdout).unwrap());
            println!("{}", String::from_utf8(output.stderr).unwrap());
        }
        Err(error) => println!("Error downloading: {}", error),
    }
    println!("Done!");
}
