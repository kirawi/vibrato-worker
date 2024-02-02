use std::{
    env::set_var,
    io::{self, stdin, stdout, BufRead, Read, Write},
    str::FromStr,
};

use serde_json::{json, to_writer, Value};
use vibrato::{tokenizer::worker::Worker, Dictionary, Tokenizer};

use anyhow::Result;

#[ouroboros::self_referencing]
struct VibratoWorker {
    tokenizer: Tokenizer,
    #[borrows(tokenizer)]
    #[covariant]
    inner: Worker<'this>,
}

struct Token {
    surface: String,
    features: Vec<String>,
}

impl VibratoWorker {
    fn create() -> Result<Self> {
        let data = include_bytes!("dict.zst");
        let mut decoder = ruzstd::StreamingDecoder::new(data.as_slice())?;
        let mut buf = vec![];
        decoder.read_to_end(&mut buf)?;
        let dict = Dictionary::read(buf.as_slice())?;
        let tokenizer = Tokenizer::new(dict).ignore_space(true)?;
        Ok(VibratoWorkerBuilder {
            tokenizer,
            inner_builder: |tokenizer| tokenizer.new_worker(),
        }
        .build())
    }

    fn tokenize(&mut self, s: &str) -> Vec<Token> {
        self.with_inner_mut(|worker| {
            worker.reset_sentence(s);
            worker.tokenize();
        });
        let output = self.borrow_inner().token_iter().map(|tk| {
            let feature_spl = tk.feature().split(',').map(ToOwned::to_owned);
            Token {
                surface: tk.surface().to_string(),
                features: feature_spl.collect(),
            }
        });
        output.collect()
    }
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut buf = [0_u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_ne_bytes(buf))
}

fn get_message<R: Read>(r: &mut R) -> Result<Value> {
    let len = read_u32(r)?;
    let mut buf = Vec::with_capacity(len as usize);
    r.read_exact(&mut buf)?;

    let s = String::from_utf8(buf)?;
    Ok(Value::from_str(&s)?)
}

fn send_message<W: Write>(w: &mut W, msg: Value) -> Result<()> {
    to_writer(w, &msg)?;
    Ok(())
}

fn setup_logger() -> Result<()> {
    set_var("RUST_BACKTRACE", "1");
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{} {} {}] {}",
                humantime::format_rfc3339_seconds(std::time::SystemTime::now()),
                record.level(),
                record.target(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .chain(fern::log_file("output.log")?)
        .apply()?;
    Ok(())
}

fn main() {
    if let Err(e) = do_stuff() {
        log::error!("{}\n{}", e, e.backtrace());
    }
}

fn do_stuff() -> anyhow::Result<()> {
    setup_logger().unwrap();
    log::info!("Beginning...");
    let mut sin = stdin().lock();
    let mut sout = stdout().lock();
    let mut worker = VibratoWorker::create()?;
    loop {
        let msg = get_message(&mut sin)?;
        log::info!("Got message {msg}");
        match msg.get("action") {
            Some(req) if req == "get_version" => {
                let sequence = msg["sequence"].clone();
                let msg = json!({
                    "sequence": sequence,
                    "data": {"version": 1},
                });
                send_message(&mut sout, msg)?;
            }
            Some(req) if req == "parse_text" => {}
            _ => unreachable!(),
        }
    }
}
