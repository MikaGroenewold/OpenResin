//! `openresin` – headless slicing, file inspection and printer control.

mod net;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use openresin_core::profile::{builtin_printers, find_printer, PrintSettings, PrinterProfile};
use openresin_core::scene::Scene;
use openresin_core::support::{auto_points, SupportSettings};
use openresin_formats::{open, slice_to_file, ExportJob, Format};

#[derive(Parser)]
#[command(
    name = "openresin",
    version,
    about = "Open-source MSLA resin slicer and printer manager"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List known printers.
    Printers {
        /// Only show printers whose name contains this text.
        filter: Option<String>,
    },
    /// Slice one or more models into a printer file.
    Slice(SliceArgs),
    /// Show information about a sliced file.
    Info { file: PathBuf },
    /// Export a layer of a sliced file as PNG.
    Layer {
        file: PathBuf,
        index: u32,
        #[arg(short, long)]
        output: PathBuf,
        /// Downscale so that the image fits into this many pixels per side.
        #[arg(long)]
        max_size: Option<u32>,
    },
    #[command(flatten)]
    Net(net::NetCmd),
}

#[derive(Args)]
struct SliceArgs {
    /// Model files (STL, OBJ, 3MF).
    #[arg(required = true)]
    models: Vec<PathBuf>,
    /// Printer id or name (see `openresin printers`).
    #[arg(short, long, default_value = "generic-4k")]
    printer: String,
    /// Output file; the extension selects the format.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Output format (goo, ctb, sl1, zip). Defaults to the printer's preferred format.
    #[arg(short, long)]
    format: Option<String>,
    /// Print settings JSON file (see `PrintSettings`).
    #[arg(long)]
    settings: Option<PathBuf>,
    #[arg(long)]
    layer_height: Option<f32>,
    #[arg(long)]
    exposure: Option<f32>,
    #[arg(long)]
    bottom_exposure: Option<f32>,
    #[arg(long)]
    bottom_layers: Option<u32>,
    /// Anti-aliasing level (1, 2, 4, 8, 16).
    #[arg(long)]
    aa: Option<u8>,
    /// Generate supports automatically (lifts the model).
    #[arg(long)]
    supports: bool,
    /// Support lift height in mm.
    #[arg(long)]
    lift: Option<f32>,
    /// Lay each model on its largest flat face before slicing.
    #[arg(long)]
    auto_orient: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Printers { filter } => {
            let f = filter.unwrap_or_default().to_lowercase();
            for p in builtin_printers()
                .iter()
                .filter(|p| p.display_name().to_lowercase().contains(&f))
            {
                println!(
                    "{:<45} {:>5}x{:<5} {:>7.2}x{:<7.2}x{:<6.1} {:<10} {}",
                    p.id,
                    p.resolution_x,
                    p.resolution_y,
                    p.size_x,
                    p.size_y,
                    p.size_z,
                    p.formats.join(","),
                    p.network.map(|n| format!("{n:?}")).unwrap_or_default()
                );
            }
            Ok(())
        }
        Cmd::Slice(args) => slice(args),
        Cmd::Info { file } => info(&file),
        Cmd::Layer {
            file,
            index,
            output,
            max_size,
        } => layer(&file, index, &output, max_size),
        Cmd::Net(cmd) => tokio::runtime::Runtime::new()?.block_on(net::run(cmd)),
    }
}

fn load_printer(query: &str) -> Result<PrinterProfile> {
    let list = builtin_printers();
    find_printer(&list, query)
        .cloned()
        .with_context(|| format!("unknown printer '{query}' (see `openresin printers`)"))
}

fn slice(a: SliceArgs) -> Result<()> {
    let printer = load_printer(&a.printer)?;
    let mut settings = match &a.settings {
        Some(p) => {
            let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            serde_json::from_str(&text).with_context(|| format!("invalid settings file {}", p.display()))?
        }
        None => PrintSettings::default(),
    };
    if let Some(v) = a.layer_height {
        settings.layer_height = v;
    }
    if let Some(v) = a.exposure {
        settings.normal.exposure = v;
    }
    if let Some(v) = a.bottom_exposure {
        settings.bottom.exposure = v;
    }
    if let Some(v) = a.bottom_layers {
        settings.bottom_layers = v;
    }
    if let Some(v) = a.aa {
        settings.anti_aliasing = v;
    }
    // check before loading and slicing the models
    settings
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid print settings: {e}"))?;
    let format = match (&a.format, &a.output) {
        (Some(f), _) => Format::from_extension(f).with_context(|| format!("unknown format '{f}'"))?,
        (None, Some(o)) => o
            .extension()
            .and_then(|e| e.to_str())
            .and_then(Format::from_extension)
            .with_context(|| format!("cannot infer format from {}", o.display()))?,
        (None, None) => printer
            .formats
            .iter()
            .find_map(|f| Format::from_extension(f))
            .unwrap_or(Format::Zip),
    };
    if !printer.formats.iter().any(|f| f == format.extension()) {
        eprintln!(
            "warning: {} is not a format listed for {} (supported: {})",
            format.extension(),
            printer.display_name(),
            printer.formats.join(", ")
        );
    }

    let mut scene = Scene::default();
    for m in &a.models {
        for model in openresin_core::io::load_models(m).with_context(|| format!("loading {}", m.display()))? {
            let id = scene.add(model.name, model.mesh);
            if a.auto_orient {
                let o = scene.get_mut(id).unwrap();
                o.transform.rotation = o.mesh.best_flat_rotation();
                scene.drop_to(id, 0.0);
            }
        }
    }
    if scene.objects.len() > 1 && !scene.arrange(&printer, 3.0) {
        eprintln!("warning: models do not all fit on the plate");
    }
    if a.supports {
        let ids: Vec<_> = scene.objects.iter().map(|o| o.id).collect();
        for id in ids {
            let mut ss = SupportSettings::default();
            if let Some(l) = a.lift {
                ss.lift_height = l;
            }
            scene.drop_to(id, ss.lift_height);
            let o = scene.get_mut(id).unwrap();
            o.support_settings = ss.clone();
            let pts = auto_points(&o.world_mesh(), &ss);
            eprintln!("{}: {} support points", o.name, pts.len());
            o.set_world_support_points(&pts);
        }
    }
    let oob = scene.out_of_bounds(&printer);
    if !oob.is_empty() {
        bail!(
            "{} model(s) do not fit into the build volume of {}",
            oob.len(),
            printer.display_name()
        );
    }

    let (meshes, negatives) = scene.slice_volumes();
    let refs: Vec<_> = meshes.iter().collect();
    let neg_refs: Vec<_> = negatives.iter().collect();
    let thumb = openresin_formats::preview::render_meshes(&refs, 400, 400);
    let first = a.models[0]
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("job")
        .to_string();
    let output = a
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{first}.{}", format.extension())));
    let job = ExportJob {
        meshes: &refs,
        negatives: &neg_refs,
        printer: &printer,
        settings: &settings,
        format,
        thumbnail: Some(&thumb),
        job_name: &first,
    };
    let res = slice_to_file(
        &job,
        &output,
        &|p| eprint!("\rslicing {}/{} layers", p.done, p.total),
        &AtomicBool::new(false),
    )?;
    eprintln!();
    let e = &res.estimate;
    println!(
        "{} → {} ({} layers, {:.1} MB) in {:.1}s",
        printer.display_name(),
        res.path.display(),
        e.layer_count,
        res.file_size as f64 / 1e6,
        res.duration.as_secs_f64()
    );
    println!(
        "estimated print time {}, resin {:.1} ml ({:.1} g, {:.2} {})",
        fmt_duration(e.print_time_s),
        e.volume_ml(),
        e.weight_g,
        e.cost,
        e.currency
    );
    Ok(())
}

fn fmt_duration(s: f64) -> String {
    let s = s.round() as u64;
    format!("{}h {:02}m {:02}s", s / 3600, (s / 60) % 60, s % 60)
}

fn info(file: &Path) -> Result<()> {
    let f = open(file).with_context(|| format!("opening {}", file.display()))?;
    println!("{}", serde_json::to_string_pretty(f.info())?);
    Ok(())
}

fn layer(file: &Path, index: u32, output: &Path, max: Option<u32>) -> Result<()> {
    let f = open(file).with_context(|| format!("opening {}", file.display()))?;
    let l = f
        .layer(index)
        .with_context(|| format!("reading layer {index} of {}", file.display()))?;
    let (w, h, px) = match max {
        Some(m) => l.downscale(m, m),
        None => (l.width, l.height, l.to_gray()),
    };
    image::GrayImage::from_raw(w, h, px)
        .context("image size")?
        .save(output)
        .with_context(|| format!("writing {}", output.display()))?;
    println!(
        "layer {index} (z = {:.3} mm, lit bbox {:?}) → {}",
        f.layer_z(index),
        l.bbox,
        output.display()
    );
    Ok(())
}
