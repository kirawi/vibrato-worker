use std::{
    env::set_var,
    io::{stdin, stdout, BufRead, Read, Write},
    path::PathBuf,
    str::FromStr,
};

use serde_json::{json, Value};
use vibrato::{tokenizer::worker::Worker, Dictionary, Tokenizer};

use memmap2::Mmap;

use anyhow::Result;

#[ouroboros::self_referencing]
struct VibratoWorker {
    mmap: Mmap,
    tokenizer: Tokenizer,
    #[borrows(tokenizer)]
    #[covariant]
    inner: Worker<'this>,
}

impl VibratoWorker {
    fn create(p: PathBuf) -> Result<Self> {
        let out = format!("{}.dump", p.to_string_lossy());
        if !p.ends_with(".dump") {
            let file = std::fs::File::open(p.as_path())?;

            let mut decoder = ruzstd::StreamingDecoder::new(file)?;
            let mut out = std::fs::File::create(&out)?;
            std::io::copy(&mut decoder, &mut out)?;
        }
        let dump = std::fs::File::open(&out)?;
        let mmap = unsafe { Mmap::map(&dump)? };
        let dict = Dictionary::read(&mmap)?;
        let tokenizer = Tokenizer::new(dict);
        Ok(VibratoWorkerBuilder {
            mmap,
            tokenizer,
            inner_builder: |tokenizer| tokenizer.new_worker(),
        }
        .build())
    }

    fn tokenize(&mut self, s: &str) -> Result<Vec<Value>> {
        self.with_inner_mut(|worker| {
            worker.reset_sentence(s);
            worker.tokenize();
        });
        let tokens = self.borrow_inner().token_iter();
        let mut out = Vec::new();
        out.push(json!({"source": s}));
        for tk in tokens {
            const DATA: &[&str] = &[
                "pos",
                "pos2",
                "pos3",
                "pos4",
                "inflection_type",
                "inflection_form",
                "lemma_reading",
                "lemma",
                "expression",
                "reading",
                "expression_base",
                "reading_base",
            ];
            let feature_spl = tk.feature().split(',');
            let surface = tk.surface();
            let info = feature_spl.flat_map(|f| f.split('-')).map(|t| {
                if t == "*" {
                    None
                } else {
                    Some(t.to_string())
                }
            });
            let mut value = vec![("source".to_string(), Some(surface.to_string()))];
            value.extend(
                DATA.into_iter()
                    .map(ToString::to_string)
                    .zip(info)
                    .collect::<Vec<(String, Option<String>)>>(),
            );
            out.push(serde_json::to_value(&value)?);
        }
        Ok(out)
    }

    fn tokenize_lines(&mut self, s: &str) -> Result<Vec<Value>> {
        let mut res = Vec::new();
        for line in s.lines() {
            // const SKIP_PAT: &'static str = r"[\s\u30fb]";
            // let a_reg = Regex::new(SKIP_PAT)?;
            // let n_reg = Regex::new(&format!(r"{0}|.*?(?={0})|.*", SKIP_PAT))?;
            for part in split_words(line) {
                if part.trim().is_empty() {
                    res.push(generate_dummy_data(part));
                    continue;
                }
                res.extend(self.tokenize(part)?);
            }
        }
        Ok(res)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Category {
    Word,
    Whitespace,
}

impl Category {
    fn from(c: &char) -> Self {
        if c.is_whitespace() {
            Self::Whitespace
        } else {
            Self::Word
        }
    }
}

fn split_words(s: &str) -> impl Iterator<Item = &str> {
    let mut iter = s.chars().peekable();
    let mut category = None;
    let mut pos: usize = 0;

    std::iter::from_fn(move || {
        let mut sum = 0;
        while let Some(ch) = iter.peek() {
            match category {
                Some(cat) if Category::from(ch) == cat => {
                    sum += ch.len_utf8();
                    let _ = iter.next();
                }
                Some(_) => {
                    let new_pos = pos + sum;
                    let res = &s[pos..new_pos];
                    pos = new_pos;
                    category = None;

                    return Some(res);
                }
                None => {
                    category = Some(Category::from(ch));
                    sum += ch.len_utf8();
                    let _ = iter.next();
                }
            }
        }
        if sum == 0 {
            None
        } else {
            Some(&s[pos..pos + sum])
        }
    })
}

fn generate_dummy_data(s: &str) -> Value {
    json!({
        "source": s.to_string(),
        "ipadic": null,
        "ipadic-neologd": null,
        "unidic-mecab-translate": null,
    })
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    let mut buf = [0_u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_ne_bytes(buf))
}

fn get_message<R: BufRead + Read>(r: &mut R) -> Result<Option<Value>> {
    if r.fill_buf()?.is_empty() {
        return Ok(None);
    }
    let len = read_u32(r)?;
    log::info!("Len {len}");
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;

    let s = String::from_utf8(buf)?;
    log::info!("Received msg: `{s}`");
    Ok(Some(Value::from_str(&s)?))
}

fn send_message<W: Write>(w: &mut W, msg: Value) -> Result<()> {
    let s = msg.to_string();
    w.write(&(s.len() as u32).to_ne_bytes())?;
    w.write_all(s.as_bytes())?;
    log::info!("Sending {} bytes", s.len());
    w.flush()?;
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
    log::info!("Hello?");
    if let Err(e) = do_stuff() {
        log::error!("{}\n{}", e, e.backtrace());
    }
}

fn do_stuff() -> anyhow::Result<()> {
    setup_logger().unwrap();
    log::info!("Beginning...");

    let dict = PathBuf::from(String::from("./system.dic.zst"));
    let mut worker = VibratoWorker::create(dict)?;

    let mut sin = stdin().lock();
    let mut sout = stdout();
    loop {
        if let Some(msg) = get_message(&mut sin)? {
            match msg.get("action") {
                Some(req) if req == "get_version" => {
                    let sequence = msg["sequence"].clone();
                    let response = json!({
                        "sequence": sequence,
                        "data": {"version": 1},
                    });
                    log::info!("Sent {response}");
                    send_message(&mut sout, response)?;
                    log::info!("Message sent!")
                }
                Some(req) if req == "parse_text" => {
                    log::info!("Asked to parse text...");
                    let text = msg["params"]["text"].as_str().unwrap_or_else(|| {
                        log::info!("Unwrapped!");
                        panic!();
                    });
                    let tokenized = serde_json::to_value(worker.tokenize_lines(text)?)?;
                    log::info!("Tokens: {tokenized}");
                    let res = json!({
                        "sequence": msg["sequence"],
                        "data": tokenized,
                    });
                    send_message(&mut sout, res)?;
                }
                _ => {
                    log::error!("Unknown request");
                    unreachable!();
                }
            }
        }
    }
}
