use anyhow::{anyhow, Context, Result};
use log::info;
use nom::{
    branch::alt,
    bytes::complete::{tag, take_until, take_while},
    character::complete::{digit1, multispace0, newline},
    combinator::{map_res, opt, recognize},
    multi::{many1, separated_list0, separated_list1},
    sequence::{delimited, preceded, separated_pair},
    AsChar, IResult,
};
use std::{collections::HashMap, fs, path::Path};

#[derive(Debug, PartialEq, Clone)]
pub enum ConfigEntry {
    Comment(String),
    Command(Config),
    DTOverlay(DTOverlay),
    DTparam(DTparam),
    ConditionFilter(String),
    GpuMem(GpuMem),
}

#[derive(Debug, PartialEq, Clone)]
pub struct GpuMem {
    total_ramsize: Option<usize>,
    gpu_ramsize: usize,
    model: Option<String>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Config {
    key: String,
    value: String,
}

#[derive(Debug, PartialEq, Clone)]
pub struct DTOverlay {
    overlay: String,
    configs: Vec<Config>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct DTparam {
    configs: Vec<Config>,
}

pub struct RPiConfig {
    configs: HashMap<String, Vec<ConfigEntry>>,
}

impl DTparam {
    /// TODO: U-Bootのconfigを現在は;で結合しているが、||や&&でも結合できるよう、戻り値をVec<String>から適切なものに変更する
    fn generate_uboot_config(&self) -> Result<Vec<String>> {
        let mut commands = Vec::new();

        fn dtparam_error(key: &str, value: &str) -> Result<String> {
            Err(anyhow!("Unsupported dtparam option: {}={}", key, value))
        }

        for (key, value) in self
            .configs
            .iter()
            .map(|Config { key, value }| (key.as_ref(), value.as_ref()))
        {
            let fdt_command: String = match key {
                "act_led_trigger" => match value {
                    "default-on" => {
                        Ok("fdt set /leds/act linux,default-trigger default-on".to_string())
                    }
                    _ => dtparam_error(&key, &value),
                },
                "audio" => match value {
                    "on" => Ok("fdt set /soc/audio status okay".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "i2c_arm" => match value {
                    "on" => Ok("fdt set i2c_arm status okay".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "i2s" => match value {
                    "on" => Ok("fdt set i2s status okay".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "pwr_led_activelow" => match value {
                    // https://patchwork.ozlabs.org/project/uboot/patch/1496149544-32348-1-git-send-email-hannes.schmelzer@br-automation.com/
                    "off" => Ok("fdt set /leds/pwr gpios < ? ? 0x00 >".to_string()),
                    "on" => Ok("fdt set /leds/pwr gpios < ? ? 0x01 >".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "pwr_led_trigger" => match value {
                    "none" => Ok("fdt set /leds/pwr linux,default-trigger none".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "spi" => match value {
                    "on" => Ok("fdt set spi0 status okay".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "watchdog" => match value {
                    "on" => Ok("fdt set watchdog status okay".to_string()),
                    _ => dtparam_error(&key, &value),
                },
                "i2c_arm_baudrate" => {
                    let baudrate: u32 = value
                        .parse()
                        .map_err(|err| anyhow!("Invalid i2c clock-frequency: {}", err))?;
                    Ok(format!("fdt set i2c clock-frequency < {:#x} >", baudrate))
                }
                _ => Err(anyhow!("Unsupported dtparam key: {}", key)),
            }?;
            commands.push(fdt_command);
        }

        Ok(commands)
    }
}

impl DTOverlay {
    fn generate_uboot_config(&self) -> Result<Vec<String>> {
        let overlay = &self.overlay;
        let configs = &self.configs;
        let mut commands: Vec<String> = Vec::new();

        // TODO: 5.x系に上げる際に読み替えるコードを追加する
        // 例: pi3-disable-bt.dtbo -> disable-bt.dtbo

        // i2sなど特殊対応のものに対応する
        if overlay == "i2smaster" {
            commands.push("fdt set i2s status okay".to_string());
            return Ok(commands);
        }

        // TODO: ロード元のアドレスを編集できるようにする
        commands.push(format!("load ${{devtype}} ${{devnum}}:${{devpart}} ${{fdt_ovaddr}} ${{fdtdir}}/overlays/{}.dtbo", overlay));
        commands.push("fdt apply ${fdt_ovaddr}".to_string());

        if !configs.is_empty() {
            // TODO: パラメータを修正するコードを入れる
            for c in configs {
                let command = match &**overlay {
                    "dwc2" => format!("fdt set usb {} {}", c.key, c.value),
                    _ => unimplemented!("not supported overlay"),
                };
                commands.push(command);
            }
        }
        Ok(commands)
    }
}

impl GpuMem {
    fn generate_uboot_config(&self) -> Result<Vec<String>> {
        // gpu_mem_*に対応したuboot configを出す
        let mut commands: Vec<String> = Vec::new();
        // TODO: total_ramsizeが0の場合（gpu_mem=*）に対応する
        let total_ramsize = self
            .total_ramsize
            .ok_or(anyhow!("Unsupported total_ramsize"))?
            * 1024
            * 1024;
        let gpu_ramsize = self.gpu_ramsize * 1024 * 1024;
        let cpu_ramsize = total_ramsize
            .checked_sub(gpu_ramsize)
            .ok_or(anyhow!("gpu_ramsize must be smaller than total_ramsize"))?;

        match &self.model {
            Some(model) => match &**model {
                "4 Model B" | "400" | "Compute Module 4" => {
                    commands.push(format!(
                        "fdt set / memreserve < {:#x} {:#x} >",
                        cpu_ramsize, gpu_ramsize,
                    ));
                    commands.push(format!(
                        "fdt set /memory@0 reg < 0x00 0x00 {:#x} 0x00 0x40000000 0xbc000000 >",
                        cpu_ramsize
                    ));
                    Ok(commands)
                }
                "3 Model B" | "3 Model B+" | "3 Model A+" | "Compute Module 3"
                | "Compute Module 3+" => {
                    commands.push(format!(
                        "fdt set / memreserve < {:#x} {:#x} >",
                        cpu_ramsize, gpu_ramsize,
                    ));
                    commands.push(format!("fdt set /memory@0 reg < 0x00 {:#x} >", cpu_ramsize,));
                    Ok(commands)
                }
                // "Zero" | "Zero W" => todo!(),
                _ => Err(anyhow!(
                    "Unsupported platform: {:?}, command: gpu_mem",
                    model
                )),
            },
            None => Err(anyhow!("gpu_mem.model is None")),
        }
    }
}

/// config.txtを読み込んで作ったconfigをuboot向けにより細分化された状態にする関数
/// 例: confitional filterのpi3はpi3 AとB両方を指すので、両方に設定が入るように分類する
fn arrange_for_uboot(
    piconfigs: &HashMap<String, Vec<ConfigEntry>>,
) -> HashMap<String, Vec<ConfigEntry>> {
    let mut ubootconfigs: HashMap<String, Vec<ConfigEntry>> = HashMap::new();

    for kv in piconfigs.iter() {
        // raspi bootloaderの荒い分類をu-bootのもう少し細かい分類に分け直す
        // raspi model: https://www.raspberrypi.com/documentation/computers/config_txt.html#model-filters
        // uboot model: https://github.com/u-boot/u-boot/blob/master/board/raspberrypi/rpi/rpi.c#L89
        let platform = kv.0;
        let configs = kv.1;
        match &**platform {
            "all" => {
                ubootconfigs.insert("all".to_string(), (*configs).clone());
            }
            "pi3" => {
                ubootconfigs.insert("3 Model B".to_string(), (*configs).clone());
                ubootconfigs.insert("3 Model B+".to_string(), (*configs).clone());
                ubootconfigs.insert("3 Model A+".to_string(), (*configs).clone());
                ubootconfigs.insert("Compute Module 3".to_string(), (*configs).clone());
                ubootconfigs.insert("Compute Module 3+".to_string(), (*configs).clone());
            }
            "pi3+" => {
                ubootconfigs.insert("3 Model B+".to_string(), (*configs).clone());
                ubootconfigs.insert("3 Model A+".to_string(), (*configs).clone());
            }
            "pi4" => {
                ubootconfigs.insert("4 Model B".to_string(), (*configs).clone());
                ubootconfigs.insert("400".to_string(), (*configs).clone());
                ubootconfigs.insert("Compute Module 4".to_string(), (*configs).clone());
            }
            "pi0" => {
                ubootconfigs.insert("Zero".to_string(), (*configs).clone());
                ubootconfigs.insert("Zero W".to_string(), (*configs).clone());
                ubootconfigs.insert("Zero 2 W".to_string(), (*configs).clone());
            }
            "pi0w" => {
                ubootconfigs.insert("Zero W".to_string(), (*configs).clone());
                ubootconfigs.insert("Zero 2 W".to_string(), (*configs).clone());
            }
            _ => {
                // TODO: 必要ならErrを出す？
                info!("Unsupported platform: {}", platform);
            }
        }
    }

    // all以下にgpu_mem_*の設定があったら適切なmodel宛に再分類する
    // u-bootでメモリ量に応じた条件分岐ができ無さそうなので、代わりにモデルで分類するため
    // TODO: all以外に対応する
    for all_config in piconfigs.get("all").unwrap_or(&Vec::new()) {
        match all_config {
            ConfigEntry::GpuMem(gpumem) => {
                gpumem.total_ramsize.map(|total_memsize| {
                    match total_memsize {
                        // https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#old-style-revision-codes
                        256 => {
                            // unsupported
                            ()
                        }
                        512 => {
                            let platforms = ["Zero", "Zero W", "3 Model A+"];
                            for platform in platforms {
                                ubootconfigs.get_mut(platform).map(|x| {
                                    x.push(ConfigEntry::GpuMem(GpuMem {
                                        total_ramsize: Some(total_memsize),
                                        gpu_ramsize: gpumem.gpu_ramsize,
                                        model: Some(platform.to_string()),
                                    }))
                                });
                            }
                        }
                        1024 => {
                            let platforms = [
                                "3 Model B",
                                "3 Model B+",
                                "Compute Module 3",
                                "Compute Module 3+",
                                "4 Model B",
                                "400",
                                "Compute Module 4",
                            ];
                            for platform in platforms {
                                let entry = ConfigEntry::GpuMem(GpuMem {
                                    total_ramsize: Some(total_memsize),
                                    gpu_ramsize: gpumem.gpu_ramsize,
                                    model: Some(platform.to_string()),
                                });
                                match ubootconfigs.get_mut(platform) {
                                    Some(x) => x.push(entry),
                                    None => {
                                        ubootconfigs.insert(platform.to_string(), vec![entry]);
                                    }
                                }
                            }
                        }
                        _ => (),
                    }
                });
                // allからは設定を削除する
                ubootconfigs
                    .get_mut("all")
                    .map(|x| x.retain(|y| y != all_config));
            }
            _ => (),
        }
    }

    ubootconfigs
}

impl RPiConfig {
    pub fn new() -> Self {
        RPiConfig {
            configs: HashMap::new(),
        }
    }

    /// /boot/config.txt から RasPiの設定を読み込む
    pub fn load_from_config(src: &Path) -> Result<Self> {
        let config = fs::read_to_string(src)
            .with_context(|| format!("Failed to read config.txt from {}", src.display()))?;
        // TODO: restに余りがあったらエラーにする
        let (_, configs) = parse(&config)
            .map_err(|err| anyhow::anyhow!("Failed to parse config.txt: {:?}", err))?;
        Ok(Self { configs })
    }

    /// configsの中身を読んで u-boot 向けのconfigを出力する
    pub fn convert_to_uboot_config(&self, envval_name: &str) -> Result<Option<String>> {
        if self.configs.is_empty() {
            return Ok(None);
        }

        let configs = arrange_for_uboot(&self.configs);

        let mut commands: Vec<String> = Vec::new();

        // 項目追加時に必要なので、fdtのアドレスを伸長する
        commands.push("setexpr fdt_ovaddr ${fdt_addr} + 0x40000".to_string());
        commands.push("fdt addr ${fdt_addr}".to_string());
        commands.push("fdt resize 0x2000".to_string());
        // dtoverlay or dtparamの設定を抜き出す
        // 全ボード向けのdtoverlay or dtparam を設定する
        // 順番が大切な部分もあるので、必ずallが最初に来るようにすること
        let supported_platforms = vec![
            "all",
            "Zero",
            "Zero W",
            "3 Model A+",
            "3 Model B",
            "3 Model B+",
            "Compute Module 3",
            "Compute Module 3+",
            "4 Model B",
            "400",
            "Compute Module 4",
        ];
        for platform in supported_platforms {
            let platform_configs = match configs.get(platform) {
                None => continue,
                Some(x) => x,
            };

            let mut tmp_commands: Vec<String> = Vec::new();

            for config in platform_configs {
                // U-Bootで設定が必要な部分を取り出して変換する
                match config {
                    ConfigEntry::DTOverlay(x) => {
                        tmp_commands.append(&mut x.generate_uboot_config()?)
                    }
                    ConfigEntry::DTparam(x) => tmp_commands.append(&mut x.generate_uboot_config()?),
                    ConfigEntry::GpuMem(x) => tmp_commands.append(&mut x.generate_uboot_config()?),
                    _ => (),
                }
            }
            if !tmp_commands.is_empty() {
                if platform == "all" {
                    commands.append(&mut tmp_commands);
                } else {
                    commands.push(format!("if test \"${{board_name}}\" = \"{}\"", platform));
                    commands.push("then".to_string());
                    commands.append(&mut tmp_commands);
                    commands.push("fi".to_string());
                }
            }
        }
        // TODO: VC memoryの設定を行う
        // シリアル番号の設定を行う
        commands.push("fdt mknode / system".to_string());
        commands.push("fdt set /system linux,revision < ${board_revision} >".to_string());

        Ok(match commands.is_empty() {
            true => None,
            false => Some(format!("{}={}", envval_name, commands.join(";"))),
        })
    }
}

fn comment(i: &str) -> IResult<&str, ConfigEntry> {
    // TODO: spaceを捨てる
    let (rest, comment) = preceded(
        tag("#"),
        take_while(|c: char| c.is_ascii() && !c.is_ascii_control()),
    )(i)?;
    let (rest, _) = take_while(|c: char| c.is_ascii_control())(rest)?;

    Ok((rest, ConfigEntry::Comment(comment.to_string())))
}

fn config(i: &str) -> IResult<&str, Config> {
    // =の左右を取り出す
    let (rest, (key, value)) = separated_pair(
        take_while(|c: char| c != '='),
        tag("="),
        take_while(|c: char| c.is_ascii() && !c.is_ascii_control()),
    )(i)?;

    let (rest, _) = take_while(|c: char| c.is_ascii_control())(rest)?;

    Ok((
        rest,
        Config {
            key: key.to_string(),
            value: value.to_string(),
        },
    ))
}

fn command(i: &str) -> IResult<&str, ConfigEntry> {
    let (rest, config) = config(i)?;

    Ok((rest, ConfigEntry::Command(config)))
}

fn dtoverlay(i: &str) -> IResult<&str, ConfigEntry> {
    // dtoverlay=spi0-1cs,cs0_pin=7,cs1_spidev=disabled

    // 先頭がdtoverlay=なら改行が来るまで読み込む
    // ,で分割
    // 最初を除いて=で分割してvecに入れる
    let (rest, mut dtoverlays_str): (&str, Vec<&str>) = delimited(
        tag("dtoverlay="),
        separated_list0(
            tag(","),
            take_while(|c: char| c.is_ascii() && c != ',' && !c.is_ascii_control()),
        ),
        multispace0,
    )(i)?;
    let overlay = dtoverlays_str.remove(0).to_string();
    let mut configs: Vec<Config> = Vec::new();
    for c in dtoverlays_str {
        let config = config(c)?;
        configs.push(config.1);
    }

    Ok((rest, ConfigEntry::DTOverlay(DTOverlay { overlay, configs })))
}

fn dtparam(i: &str) -> IResult<&str, ConfigEntry> {
    // dtparam=i2c_arm=on
    let (rest, dtparams_str) = delimited(
        tag("dtparam="),
        separated_list1(
            tag(","),
            take_while(|c: char| c.is_ascii() && c != ',' && !c.is_ascii_control()),
        ),
        multispace0,
    )(i)?;
    let mut configs: Vec<Config> = Vec::new();
    for c in dtparams_str {
        let config = config(c)?;
        configs.push(config.1);
    }

    Ok((rest, ConfigEntry::DTparam(DTparam { configs })))
}

fn gpumem(i: &str) -> IResult<&str, ConfigEntry> {
    let (rest, gpumem_str) = delimited(tag("gpu_mem="), digit1, multispace0)(i)?;
    let memsize: (&str, usize) = map_res(recognize(digit1), str::parse)(gpumem_str)?;
    let gpumem = ConfigEntry::GpuMem(GpuMem {
        total_ramsize: None,
        gpu_ramsize: memsize.1,
        model: None,
    });
    Ok((rest, gpumem))
}

fn gpumem_condition(i: &str) -> IResult<&str, ConfigEntry> {
    let (rest, gpumem_str) = delimited(
        tag("gpu_mem_"),
        separated_list1(
            tag("="),
            take_while(|c: char| c.is_dec_digit() && c != '=' && !c.is_ascii_control()),
        ),
        multispace0,
    )(i)?;

    let total_memsize: (&str, usize) = map_res(recognize(digit1), str::parse)(gpumem_str[0])?;
    let gpu_memsize: (&str, usize) = map_res(recognize(digit1), str::parse)(gpumem_str[1])?;

    let gpumem = ConfigEntry::GpuMem(GpuMem {
        total_ramsize: Some(total_memsize.1),
        gpu_ramsize: gpu_memsize.1,
        model: None,
    });
    Ok((rest, gpumem))
}

fn condition_filter(i: &str) -> IResult<&str, ConfigEntry> {
    let (rest, filter) = delimited(tag("["), take_until("]"), tag("]"))(i)?;
    Ok((rest, ConfigEntry::ConditionFilter(filter.to_string())))
}

fn config_entry(i: &str) -> IResult<&str, ConfigEntry> {
    let (rest, entry): (&str, ConfigEntry) = alt((
        condition_filter,
        comment,
        dtoverlay,
        dtparam,
        gpumem,
        gpumem_condition,
        command,
    ))(i)?;
    Ok((rest, entry))
}

fn config_list(i: &str) -> IResult<&str, Vec<ConfigEntry>> {
    many1(preceded(opt(newline), config_entry))(i)
}

fn parse(i: &str) -> IResult<&str, HashMap<String, Vec<ConfigEntry>>> {
    let (rest, configs) = config_list(i)?;

    // filterでまとめる
    let mut key = "all".to_string();
    let mut result: HashMap<String, Vec<ConfigEntry>> = HashMap::new();
    result.insert(key.clone(), vec![]);

    for config in configs {
        match config {
            ConfigEntry::ConditionFilter(c) => {
                key = c;
                if !result.contains_key(&key) {
                    result.insert(key.clone(), vec![]);
                }
            }
            _ => {
                if let Some(c) = result.get_mut(&key) {
                    c.push(config)
                }
            }
        }
    }

    Ok((rest, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dtoverlay_uboot() {
        let expected: Vec<(DTOverlay, Vec<String>)> = vec![
            (
                DTOverlay {
                    overlay: "pi3-disable-bt".to_string(),
                    configs: vec![],
                },
                vec!["load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/pi3-disable-bt.dtbo",
                "fdt apply ${fdt_ovaddr}",
                ].iter_mut().map(|x| x.to_string()).collect(),
            ),
            (
                DTOverlay {
                    overlay: "pi3-disable-wifi".to_string(),
                    configs: vec![],
                },
                vec!["load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/pi3-disable-wifi.dtbo",
                "fdt apply ${fdt_ovaddr}",
                ].iter_mut().map(|x| x.to_string()).collect(),
            ),
            (
                DTOverlay {
                    overlay: "disable-bt".to_string(),
                    configs: vec![],
                },
                vec!["load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/disable-bt.dtbo",
                "fdt apply ${fdt_ovaddr}",
                ].iter_mut().map(|x| x.to_string()).collect(),
            ),
            (
                DTOverlay {
                    overlay: "dwc2".to_string(),
                    configs: vec![Config {
                        key: "dr_mode".to_string(),
                        value: "host".to_string(),
                    }],
                },
                vec!["load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/dwc2.dtbo",
                "fdt apply ${fdt_ovaddr}",
                "fdt set usb dr_mode host",
                ].iter_mut().map(|x| x.to_string()).collect(),
            ),
            (
                DTOverlay {
                    overlay: "i2smaster".to_string(),
                    configs: vec![],
                },
                vec!["fdt set i2s status okay"].iter_mut().map(|x| x.to_string()).collect(),
            ),
            (
                DTOverlay {
                    overlay: "vc4-fkms-v3d".to_string(),
                    configs: vec![],
                },
                vec!["load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/vc4-fkms-v3d.dtbo",
                "fdt apply ${fdt_ovaddr}",
                ].iter_mut().map(|x| x.to_string()).collect(),
            ),
        ];

        for tmp in expected {
            let dtbo = tmp.0;
            let expected = tmp.1;

            let result = dtbo.generate_uboot_config().unwrap();
            assert_eq!(expected, result);
        }
    }

    #[test]
    fn test_dtparam_uboot() {
        let expected: Vec<(DTparam, Vec<String>)> = vec![
            (
                DTparam {
                    configs: vec![Config {
                        key: "act_led_trigger".to_string(),
                        value: "default-on".to_string(),
                    }],
                },
                vec!["fdt set /leds/act linux,default-trigger default-on"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "audio".to_string(),
                        value: "on".to_string(),
                    }],
                },
                vec!["fdt set /soc/audio status okay"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "i2c_arm".to_string(),
                        value: "on".to_string(),
                    }],
                },
                vec!["fdt set i2c_arm status okay"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "i2s".to_string(),
                        value: "on".to_string(),
                    }],
                },
                vec!["fdt set i2s status okay"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "pwr_led_activelow".to_string(),
                        value: "off".to_string(),
                    }],
                },
                vec!["fdt set /leds/pwr gpios < ? ? 0x00 >"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "pwr_led_activelow".to_string(),
                        value: "on".to_string(),
                    }],
                },
                vec!["fdt set /leds/pwr gpios < ? ? 0x01 >"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "pwr_led_trigger".to_string(),
                        value: "none".to_string(),
                    }],
                },
                vec!["fdt set /leds/pwr linux,default-trigger none"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "spi".to_string(),
                        value: "on".to_string(),
                    }],
                },
                vec!["fdt set spi0 status okay"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "watchdog".to_string(),
                        value: "on".to_string(),
                    }],
                },
                vec!["fdt set watchdog status okay"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
            (
                DTparam {
                    configs: vec![Config {
                        key: "i2c_arm_baudrate".to_string(),
                        value: "400000".to_string(),
                    }],
                },
                vec!["fdt set i2c clock-frequency < 0x61a80 >"]
                    .iter_mut()
                    .map(|x| x.to_string())
                    .collect(),
            ),
        ];

        for tmp in expected {
            let dtparam = tmp.0;
            let expected = tmp.1;

            let result = dtparam.generate_uboot_config().unwrap();
            assert_eq!(expected, result);
        }
    }

    // RPiConfig
    #[test]
    fn test_convert_to_uboot_config() {
        let rpiconfig = RPiConfig {
            configs: HashMap::from([
                (
                    "all".to_string(),
                    vec![ConfigEntry::DTparam(DTparam {
                        configs: vec![Config {
                            key: "audio".to_string(),
                            value: "on".to_string(),
                        }],
                    })],
                ),
                (
                    "pi4".to_string(),
                    vec![ConfigEntry::DTOverlay(DTOverlay {
                        overlay: "vc4-fkms-v3d".to_string(),
                        configs: vec![],
                    })],
                ),
            ]),
        };
        let expected = vec!["setexpr fdt_ovaddr ${fdt_addr} + 0x40000",
        "fdt addr ${fdt_addr}",
        "fdt resize 0x2000",
        "fdt set /soc/audio status okay",
        "if test \"${board_name}\" = \"4 Model B\"",
        "then",
        "load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/vc4-fkms-v3d.dtbo",
        "fdt apply ${fdt_ovaddr}",
        "fi",
        "if test \"${board_name}\" = \"400\"",
        "then",
        "load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/vc4-fkms-v3d.dtbo",
        "fdt apply ${fdt_ovaddr}",
        "fi",
        "if test \"${board_name}\" = \"Compute Module 4\"",
        "then",
        "load ${devtype} ${devnum}:${devpart} ${fdt_ovaddr} ${fdtdir}/overlays/vc4-fkms-v3d.dtbo",
        "fdt apply ${fdt_ovaddr}",
        "fi",
        "fdt mknode / system",
        "fdt set /system linux,revision < ${board_revision} >"];
        let expected = format!("bootconfig={}", expected.join(";"));

        let output = rpiconfig
            .convert_to_uboot_config("bootconfig")
            .unwrap()
            .unwrap();
        assert_eq!(expected, output);

        // TODO: gpu_memの設定を入れる

        let rpiconfig = RPiConfig {
            configs: HashMap::from([(
                "all".to_string(),
                vec![ConfigEntry::GpuMem(GpuMem {
                    total_ramsize: Some(1024),
                    gpu_ramsize: 128,
                    model: None,
                })],
            )]),
        };
        let expected = vec![
            "setexpr fdt_ovaddr ${fdt_addr} + 0x40000",
            "fdt addr ${fdt_addr}",
            "fdt resize 0x2000",
            "if test \"${board_name}\" = \"3 Model B\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x38000000 >",
            "fi",
            "if test \"${board_name}\" = \"3 Model B+\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x38000000 >",
            "fi",
            "if test \"${board_name}\" = \"Compute Module 3\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x38000000 >",
            "fi",
            "if test \"${board_name}\" = \"Compute Module 3+\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x38000000 >",
            "fi",
            "if test \"${board_name}\" = \"4 Model B\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x00 0x38000000 0x00 0x40000000 0xbc000000 >",
            "fi",
            "if test \"${board_name}\" = \"400\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x00 0x38000000 0x00 0x40000000 0xbc000000 >",
            "fi",
            "if test \"${board_name}\" = \"Compute Module 4\"",
            "then",
            "fdt set / memreserve < 0x38000000 0x8000000 >",
            "fdt set /memory@0 reg < 0x00 0x00 0x38000000 0x00 0x40000000 0xbc000000 >",
            "fi",
            "fdt mknode / system",
            "fdt set /system linux,revision < ${board_revision} >",
        ];

        let expected = format!("bootconfig={}", expected.join(";"));

        let output = rpiconfig
            .convert_to_uboot_config("bootconfig")
            .unwrap()
            .unwrap();
        assert_eq!(expected, output);
    }

    // parser
    #[test]
    fn test_comment() {
        assert_eq!(
            comment("# comment"),
            Ok(("", ConfigEntry::Comment(" comment".to_string())))
        );
        assert_eq!(
            comment("# comment\n#hogehoge"),
            Ok(("#hogehoge", ConfigEntry::Comment(" comment".to_string())))
        );
    }

    #[test]
    fn test_command() {
        assert_eq!(
            command("enable_uart=1"),
            Ok((
                "",
                ConfigEntry::Command(Config {
                    key: "enable_uart".to_string(),
                    value: "1".to_string()
                })
            ))
        );

        assert_eq!(
            command("enable_uart=1\narm_freq=800"),
            Ok((
                "arm_freq=800",
                ConfigEntry::Command(Config {
                    key: "enable_uart".to_string(),
                    value: "1".to_string()
                })
            ))
        );
    }

    #[test]
    fn test_dtoverlay() {
        assert_eq!(
            dtoverlay("dtoverlay=vc4-fkms-v3d"),
            Ok((
                "",
                ConfigEntry::DTOverlay(DTOverlay {
                    overlay: "vc4-fkms-v3d".to_string(),
                    configs: vec![]
                })
            ))
        );
        assert_eq!(
            dtoverlay("dtoverlay=vc4-fkms-v3d\ndtoverlay=dwc2"),
            Ok((
                "dtoverlay=dwc2",
                ConfigEntry::DTOverlay(DTOverlay {
                    overlay: "vc4-fkms-v3d".to_string(),
                    configs: vec![]
                })
            ))
        );

        assert_eq!(
            dtoverlay("dtoverlay=spi0-1cs,cs0_pin=7,cs1_spidev=disabled"),
            Ok((
                "",
                ConfigEntry::DTOverlay(DTOverlay {
                    overlay: "spi0-1cs".to_string(),
                    configs: vec![
                        Config {
                            key: "cs0_pin".to_string(),
                            value: "7".to_string()
                        },
                        Config {
                            key: "cs1_spidev".to_string(),
                            value: "disabled".to_string()
                        }
                    ]
                })
            ))
        );
    }

    #[test]
    fn test_dtparam() {
        assert_eq!(
            dtparam("dtparam=i2c_arm=on"),
            Ok((
                "",
                ConfigEntry::DTparam(DTparam {
                    configs: vec![Config {
                        key: "i2c_arm".to_string(),
                        value: "on".to_string()
                    }]
                })
            ))
        );

        assert_eq!(
            dtparam("dtparam=i2c_arm=on,spi=on"),
            Ok((
                "",
                ConfigEntry::DTparam(DTparam {
                    configs: vec![
                        Config {
                            key: "i2c_arm".to_string(),
                            value: "on".to_string()
                        },
                        Config {
                            key: "spi".to_string(),
                            value: "on".to_string()
                        }
                    ]
                })
            ))
        );
    }

    #[test]
    fn test_condition_filter() {
        assert_eq!(
            condition_filter("[pi4]"),
            Ok(("", ConfigEntry::ConditionFilter("pi4".to_string())))
        );
        assert_eq!(
            condition_filter("[all]"),
            Ok(("", ConfigEntry::ConditionFilter("all".to_string())))
        );
    }

    #[test]
    fn test_config() {
        let text = r"dtparam=audio=on

[pi4]
# Enable DRM VC4 V3D driver on top of the dispmanx display stack
dtoverlay=vc4-fkms-v3d
max_framebuffers=2

[all]
#dtoverlay=vc4-fkms-v3d
enable_uart=1
dtparam=i2c_arm=on
dtoverlay=dwc2,dr_mode=host
dtoverlay=spi0-1cs,cs0_pin=7,cs1_spidev=disabled
";

        let expected = vec![
            ConfigEntry::DTparam(DTparam {
                configs: vec![Config {
                    key: "audio".to_string(),
                    value: "on".to_string(),
                }],
            }),
            ConfigEntry::ConditionFilter("pi4".to_string()),
            ConfigEntry::Comment(
                " Enable DRM VC4 V3D driver on top of the dispmanx display stack".to_string(),
            ),
            ConfigEntry::DTOverlay(DTOverlay {
                overlay: "vc4-fkms-v3d".to_string(),
                configs: vec![],
            }),
            ConfigEntry::Command(Config {
                key: "max_framebuffers".to_string(),
                value: "2".to_string(),
            }),
            ConfigEntry::ConditionFilter("all".to_string()),
            ConfigEntry::Comment("dtoverlay=vc4-fkms-v3d".to_string()),
            ConfigEntry::Command(Config {
                key: "enable_uart".to_string(),
                value: "1".to_string(),
            }),
            ConfigEntry::DTparam(DTparam {
                configs: vec![Config {
                    key: "i2c_arm".to_string(),
                    value: "on".to_string(),
                }],
            }),
            ConfigEntry::DTOverlay(DTOverlay {
                overlay: "dwc2".to_string(),
                configs: vec![Config {
                    key: "dr_mode".to_string(),
                    value: "host".to_string(),
                }],
            }),
            ConfigEntry::DTOverlay(DTOverlay {
                overlay: "spi0-1cs".to_string(),
                configs: vec![
                    Config {
                        key: "cs0_pin".to_string(),
                        value: "7".to_string(),
                    },
                    Config {
                        key: "cs1_spidev".to_string(),
                        value: "disabled".to_string(),
                    },
                ],
            }),
        ];

        assert_eq!(config_list(text), Ok(("", expected)));
    }

    #[test]
    fn test_parse() {
        let text = r"dtparam=audio=on

[pi4]
# Enable DRM VC4 V3D driver on top of the dispmanx display stack
dtoverlay=vc4-fkms-v3d
max_framebuffers=2

[all]
#dtoverlay=vc4-fkms-v3d
enable_uart=1
dtparam=i2c_arm=on
dtoverlay=dwc2,dr_mode=host
dtoverlay=spi0-1cs,cs0_pin=7,cs1_spidev=disabled
";

        let expected = HashMap::from([
            (
                "all".to_string(),
                vec![
                    ConfigEntry::DTparam(DTparam {
                        configs: vec![Config {
                            key: "audio".to_string(),
                            value: "on".to_string(),
                        }],
                    }),
                    ConfigEntry::Comment("dtoverlay=vc4-fkms-v3d".to_string()),
                    ConfigEntry::Command(Config {
                        key: "enable_uart".to_string(),
                        value: "1".to_string(),
                    }),
                    ConfigEntry::DTparam(DTparam {
                        configs: vec![Config {
                            key: "i2c_arm".to_string(),
                            value: "on".to_string(),
                        }],
                    }),
                    ConfigEntry::DTOverlay(DTOverlay {
                        overlay: "dwc2".to_string(),
                        configs: vec![Config {
                            key: "dr_mode".to_string(),
                            value: "host".to_string(),
                        }],
                    }),
                    ConfigEntry::DTOverlay(DTOverlay {
                        overlay: "spi0-1cs".to_string(),
                        configs: vec![
                            Config {
                                key: "cs0_pin".to_string(),
                                value: "7".to_string(),
                            },
                            Config {
                                key: "cs1_spidev".to_string(),
                                value: "disabled".to_string(),
                            },
                        ],
                    }),
                ],
            ),
            (
                "pi4".to_string(),
                vec![
                    ConfigEntry::Comment(
                        " Enable DRM VC4 V3D driver on top of the dispmanx display stack"
                            .to_string(),
                    ),
                    ConfigEntry::DTOverlay(DTOverlay {
                        overlay: "vc4-fkms-v3d".to_string(),
                        configs: vec![],
                    }),
                    ConfigEntry::Command(Config {
                        key: "max_framebuffers".to_string(),
                        value: "2".to_string(),
                    }),
                ],
            ),
        ]);

        assert_eq!(parse(text), Ok(("", expected)));
    }

    #[test]
    fn test_parse_led_trigger() {
        let text = r"dtparam=act_led_trigger=default-on
dtparam=pwr_led_trigger=none
dtparam=pwr_led_activelow=on
";

        let expected = HashMap::from([(
            "all".to_string(),
            vec![
                ConfigEntry::DTparam(DTparam {
                    configs: vec![Config {
                        key: "act_led_trigger".to_string(),
                        value: "default-on".to_string(),
                    }],
                }),
                ConfigEntry::DTparam(DTparam {
                    configs: vec![Config {
                        key: "pwr_led_trigger".to_string(),
                        value: "none".to_string(),
                    }],
                }),
                ConfigEntry::DTparam(DTparam {
                    configs: vec![Config {
                        key: "pwr_led_activelow".to_string(),
                        value: "on".to_string(),
                    }],
                }),
            ],
        )]);

        assert_eq!(parse(text), Ok(("", expected)));
    }

    #[test]
    fn test_parse_gpumem() {
        let text = r"gpu_mem=512
gpu_mem_1024=512
";

        let expected = HashMap::from([(
            "all".to_string(),
            vec![
                ConfigEntry::GpuMem(GpuMem {
                    total_ramsize: None,
                    gpu_ramsize: 512,
                    model: None,
                }),
                ConfigEntry::GpuMem(GpuMem {
                    total_ramsize: Some(1024),
                    gpu_ramsize: 512,
                    model: None,
                }),
            ],
        )]);

        assert_eq!(parse(text), Ok(("", expected)));
    }

    #[test]
    fn test_gpumem() {
        assert_eq!(
            gpumem("gpu_mem=512"),
            Ok((
                "",
                ConfigEntry::GpuMem(GpuMem {
                    total_ramsize: None,
                    gpu_ramsize: 512,
                    model: None,
                }),
            ))
        );
    }

    #[test]
    fn test_gpumem_condition() {
        assert_eq!(
            gpumem_condition("gpu_mem_1024=512"),
            Ok((
                "",
                ConfigEntry::GpuMem(GpuMem {
                    total_ramsize: Some(1024),
                    gpu_ramsize: 512,
                    model: None
                }),
            ))
        );
    }
}
