use std::{env, path::PathBuf, str::FromStr, fs::File, io::Write};
use anyhow::{anyhow, Context, Result};
use pibootcfg::{RPiConfig};

fn usage() {
    println!("usage:");
    println!("\tpibconfig2uboot SRC DEST");
    println!("example:");
    println!("\tpibconfig2uboot /boot/config.txt /boot/uEnv.txt");
}

fn main() -> Result<()> {
    // config.txtを読み込んでuEnvにするコマンド
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("error: not enough arguments");
        usage();
        std::process::exit(1);
    }

    let src = args.get(1).unwrap();
    match &**src {
        "?" | "h" | "help" => usage(),
        _ => ()
    };
    let dest = args.get(2).unwrap();
    let src = PathBuf::from(src);
    let dest = PathBuf::from(dest);

    let mut piconfig = RPiConfig::new();
    piconfig.load_from_config(&src)?;

    let uenv = piconfig.convert_to_uboot_config("bootcfg".to_string())?.unwrap_or(format!("bootcfg=\"echo nothing to do\""));

    let mut file = File::create(&dest).with_context(|| format!("failed to create {:?}", dest))?;
    file.write_all(uenv.as_bytes()).with_context(|| format!("failed to write u-boot config to {:?}", dest))?;

    Ok(())
}