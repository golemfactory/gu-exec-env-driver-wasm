use gu_wasm_env_api::{Manifest, MountPoint, EntryPoint};
use crate::Opt::ResolvePath;
use crate::ResolveResult::ResolvedPath;
use failure::{err_msg, Fallible, bail};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io;
use std::path::{Component, Path, PathBuf};
use structopt::StructOpt;
use sp_wasm_engine::sandbox::load::Bytes;
use sp_wasm_engine::sandbox::Sandbox;
use sp_wasm_engine::prelude::NodeMode;

#[derive(StructOpt)]
#[structopt(rename_all = "kebab-case")]
enum Opt {
    ValidateImage(ValidateImage),
    Create(Create),
    Open(Open),
    Exec(Exec),
    ResolvePath(Resolve),
}

#[derive(StructOpt)]
#[structopt(rename_all = "kebab-case")]
struct ValidateImage {
    #[structopt(parse(from_os_str))]
    image_path: PathBuf,
}

fn load_manifest(image_path: &Path) -> Fallible<Manifest> {
    let mut a = zip::ZipArchive::new(OpenOptions::new().read(true).open(image_path)?)?;

    let entry = a.by_name("gu-package.json")?;

    Ok(serde_json::from_reader(entry)?)
}

fn normalize_path(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    path.as_ref().components()
        .fold(Ok(PathBuf::from("")), |agg, part| match (agg, part) {
            (Ok(path), Component::RootDir) => Ok(path),
            (Ok(path), Component::Prefix(_)) => Ok(path),
            (Ok(path), Component::Normal(part)) => Ok(path.join(part)),
            _ => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        })
}


fn run_ep(image_path : &Path, workdir : &Path, ep : &EntryPoint, m : &Manifest, args : Vec<String>) -> Fallible<()> {
    let wasm_path = normalize_path(&ep.wasm_path)?;
    let js_path = wasm_path.with_extension("js");

    let (js_bytes, wasm_bytes) = {
        let mut a = zip::ZipArchive::new(OpenOptions::new().read(true).open(image_path)?)?;

        eprintln!("js={}, wasm={}", js_path.display(), wasm_path.display());

        let wasm = a.by_name(wasm_path.to_string_lossy().as_ref())?;
        let wasm_bytes = Bytes::from_reader(wasm)?;
        let js_bytes = Bytes::from_reader(a.by_name(js_path.to_string_lossy().as_ref())?)?;

        (js_bytes, wasm_bytes)
    };

    let mut sb = Sandbox::new()?;

    if let Some(work_dir) = &m.work_dir {
        sb = sb.work_dir(work_dir)?;
    }
    let mounts: Vec<(String, MountPoint)> =
        serde_json::from_slice(std::fs::read(workdir.join("mounts.json"))?.as_slice())?;
    sb = sb.set_exec_args(args)?;
    sb.init()?;
    sb.mount(&image_path, "@", NodeMode::Ro)?;

    for (path, mount_point) in mounts {
        sb.mount(workdir.join(path), mount_point.path(), NodeMode::Rw)?;
    }



    let _ = sb.run(js_bytes, wasm_bytes)?;

    Ok(())
}

impl ValidateImage {
    fn execute(self) -> Fallible<()> {
        // Getting image
        let mut a = zip::ZipArchive::new(OpenOptions::new().read(true).open(self.image_path)?)?;

        let entry = a.by_name("gu-package.json")?;
        let m: gu_wasm_env_api::Manifest = serde_json::from_reader(entry)?;

        eprintln!("m={:?}", m);

        Ok(())
    }
}

#[derive(StructOpt, Debug)]
#[structopt(rename_all = "kebab-case")]
struct Create {
    #[structopt(long, parse(from_os_str))]
    image: PathBuf,
    #[structopt(long, parse(from_os_str))]
    workdir: PathBuf,
    #[structopt(long, parse(from_os_str))]
    spec: PathBuf,
}

impl Create {
    fn execute(self) -> Fallible<()> {
        let m = load_manifest(&self.image)?;
        let mut args = Vec::new();
        for mount_point in m.mount_points {
            let id = uuid::Uuid::new_v4();
            let id_str = id.to_hyphenated().to_string();
            let full_path = self.workdir.join(&id_str);
            std::fs::create_dir(full_path)?;
            args.push((id_str, mount_point));
        }
        std::fs::write(
            self.workdir.join("mounts.json"),
            serde_json::to_vec_pretty(&args)?,
        )?;
        Ok(())
    }
}

#[derive(StructOpt, Debug)]
#[structopt(rename_all = "kebab-case")]
struct Resolve {
    #[structopt(long, parse(from_os_str))]
    image: PathBuf,
    #[structopt(long, parse(from_os_str))]
    workdir: PathBuf,
    #[structopt(long, parse(from_os_str))]
    spec: PathBuf,
    /// Path inside container
    destination: String,
}

impl Resolve {
    fn execute(self) -> Fallible<()> {
        eprintln!("WASM: resolve path {:?}", self);

        let mounts: Vec<(String, MountPoint)> =
            serde_json::from_slice(std::fs::read(self.workdir.join("mounts.json"))?.as_slice())?;

        let base = PathBuf::from("");
        let output = PathBuf::from(self.destination);

        let work_dir: PathBuf = normalize_path(&output)?;

        let mut result = ResolveResult::UnresolvedPath;
        for (dest, mount_point) in mounts {
            let mount_path = normalize_path(mount_point.path())?;
            if work_dir.starts_with(&mount_path) {
                result = ResolveResult::ResolvedPath(
                    self.workdir
                        .join(dest)
                        .join(work_dir.strip_prefix(&mount_path)?)
                        .display()
                        .to_string(),
                );
                break;
            } else {
                eprintln!("{} -- {}", work_dir.display(), mount_point.path())
            }
        }

        println!("{}", serde_json::to_string_pretty(&result)?);
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum ResolveResult {
    ResolvedPath(String),
    UnresolvedPath,
}

#[derive(StructOpt, Debug)]
#[structopt(rename_all = "kebab-case")]
struct Open {
    #[structopt(long, parse(from_os_str))]
    image: PathBuf,
    #[structopt(long, parse(from_os_str))]
    workdir: PathBuf,
    #[structopt(long, parse(from_os_str))]
    spec: PathBuf,
}

impl Open {
    fn execute(self) -> Fallible<()> {
       let m = load_manifest(&self.image)?;
       if let Some(main_ep) = &m.main {
           run_ep(&self.image, &self.workdir, main_ep, &m, Vec::new())?;
       }
       Ok(())
    }
}

#[derive(StructOpt, Debug)]
#[structopt(rename_all = "kebab-case")]
struct Exec {
    #[structopt(long, parse(from_os_str))]
    image: PathBuf,
    #[structopt(long, parse(from_os_str))]
    workdir: PathBuf,
    #[structopt(long, parse(from_os_str))]
    spec: PathBuf,

    prog: String,
    args: Vec<String>,
}

impl Exec {
    fn execute(self) -> Fallible<()> {
        let m = load_manifest(&self.image)?;
        if let Some(ep) = m.entry_points.iter().find(|&ep| ep.id == self.prog) {
            run_ep(&self.image, &self.workdir, ep, &m, self.args)?;
        }
        else {
            bail!("invalid entry point: {}", self.prog);
        }
        Ok(())
    }
}

fn main() {
    match Opt::from_args() {
        Opt::ValidateImage(command) => command.execute().unwrap(),
        Opt::Create(command) => command.execute().unwrap(),
        Opt::ResolvePath(command) => command.execute().unwrap(),
        Opt::Open(command) => command.execute().unwrap(),
        Opt::Exec(command) => command.execute().unwrap(),
    }
}
