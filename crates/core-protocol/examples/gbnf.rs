//! Print the intent grammar, or (with `--cases`) grammar conformance test cases.
//!
//! Cases are one per line: `+ <json>` must be accepted by the grammar, `- <json>` must
//! be rejected. They are consumed by `tools/gbnf-check`, which runs them through
//! llama.cpp's own grammar engine.

use core_protocol::CATALOG;
use core_protocol::grammar::{GrammarOptions, gbnf};

fn main() {
    if std::env::args().any(|a| a == "--cases") {
        for spec in CATALOG {
            println!("+ {}", spec.example_intent("example"));
        }
        // Spacing variants the grammar tolerates.
        println!(r#"+ {{ "thought": "", "action": "set_volume", "args": {{ "percent": 0 }} }}"#);
        println!(r#"+ {{"thought":"t","action":"read_logs","args":{{}}}}"#);
        println!(r#"+ {{"thought":"t","action":"read_logs","args":{{"lines":5}}}}"#);
        println!(r#"+ {{"thought":"t","action":"read_logs","args":{{"priority":"err","lines":5}}}}"#);
        println!(r#"+ {{"thought":"t","action":"respond","args":{{"message":"line\nbreak \"quoted\" é"}}}}"#);
        println!(r#"+ {{"thought":"t","action":"launch_program","args":{{"program":"htop","args":[]}}}}"#);
        println!(r#"+ {{"thought":"t","action":"ping_host","args":{{"host":"::1"}}}}"#);
        // Things the grammar must make impossible.
        println!(r#"- {{"thought":"t","action":"rm_rf","args":{{}}}}"#);
        println!(r#"- {{"thought":"t","action":"restart_service","args":{{"service":"a b"}}}}"#);
        println!(r#"- {{"thought":"t","action":"restart_service","args":{{"service":"-x"}}}}"#);
        println!(r#"- {{"thought":"t","action":"restart_service","args":{{}}}}"#);
        println!(r#"- {{"thought":"t","action":"reboot","args":{{"force":true}}}}"#);
        println!(r#"- {{"thought":"t","action":"install_package","args":{{"package":"--noconfirm"}}}}"#);
        println!(r#"- {{"thought":"t","action":"set_volume","args":{{"percent":007}}}}"#);
        println!(r#"- {{"thought":"t","action":"set_volume","args":{{"percent":"40"}}}}"#);
        println!(r#"- {{"thought":"t","action":"read_file","args":{{"path":"relative/path"}}}}"#);
        println!(r#"- {{"thought":"t","action":"read_logs","args":{{,"lines":5}}}}"#);
        println!(r#"- {{"thought":"t","action":"read_logs","args":{{"lines":5,}}}}"#);
        println!(r#"- {{"action":"disk_usage","args":{{}}}}"#);
        println!(r#"- Sure! {{"thought":"t","action":"disk_usage","args":{{}}}}"#);
        println!(r#"- {{"thought":"t","action":"disk_usage","args":{{}}}} extra"#);
        return;
    }
    print!("{}", gbnf(&GrammarOptions::default()));
}
