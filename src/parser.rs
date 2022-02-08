///! config.txt parser
use std::collections::HashMap;

use nom::{
    branch::alt,
    bytes::complete::{tag, take_until, take_while},
    character::complete::{digit1, multispace0, newline},
    combinator::{map_res, opt, recognize},
    multi::{many1, separated_list0, separated_list1},
    sequence::{delimited, preceded, separated_pair},
    AsChar, IResult,
};

use crate::{Config, ConfigEntry, DTOverlay, DTparam, GpuMem};

fn comment(i: &str) -> IResult<&str, ConfigEntry> {
    // TODO: spaceを捨てる
    let (rest, comment) = preceded(
        tag("#"),
        take_while(|c: char| c.is_ascii() && !c.is_ascii_control()),
    )(i)?;
    let (rest, _) = take_while(|c: char| c.is_ascii_control())(rest)?;

    Ok((rest, ConfigEntry::Comment(comment.to_string())))
}

/// =の左右を取り出す
fn config(i: &str) -> IResult<&str, Config> {
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

/// e.g. dtoverlay=spi0-1cs,cs0_pin=7,cs1_spidev=disabled
fn dtoverlay(i: &str) -> IResult<&str, ConfigEntry> {
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

/// e.g. dtparam=i2c_arm=on
fn dtparam(i: &str) -> IResult<&str, ConfigEntry> {
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

/// parse the text in config.txt
pub fn parse(i: &str) -> IResult<&str, HashMap<String, Vec<ConfigEntry>>> {
    let (rest, configs) = config_list(i)?;

    // filterでまとめる
    let mut key = "all".to_string();
    let mut result: HashMap<String, Vec<ConfigEntry>> = HashMap::from([(key.clone(), vec![])]);

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
