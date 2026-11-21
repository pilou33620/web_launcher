//! Terminal d'un web tool : lance la commande dans un pseudo-terminal (comme une fenêtre de
//! terminal), écrit tout ce qu'elle affiche dans son journal, et lui transmet ce qui est saisi
//! dans la page « Terminaux » du launcher (via une FIFO).
//!
//!     launcher --console <fifo> <journal> -- commande args...
//!
//! Ce petit processus vit à côté de l'outil, indépendamment du launcher : un redémarrage du
//! launcher ne coupe pas le terminal. Il se termine avec l'outil, avec son code de sortie.
//! L'outil a sa propre session, avec ce terminal pour terminal de contrôle (« setsid --ctty »,
//! util-linux) : un Ctrl+C saisi dans la page l'interrompt comme dans une vraie fenêtre.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;

const LIGNES: u16 = 40;
const COLONNES: u16 = 120;

pub fn main(argv: &[String]) -> i32 {
    if argv.len() < 4 || argv[2] != "--" {
        eprintln!("usage : launcher --console <fifo> <journal> -- commande...");
        return 2;
    }
    let (fifo, journal, cmd) = (&argv[0], &argv[1], &argv[3..]);
    let _ = std::fs::remove_file(fifo);
    if let Err(e) = nix::unistd::mkfifo(fifo.as_str(), nix::sys::stat::Mode::from_bits_truncate(0o600)) {
        eprintln!("Terminal impossible ({}) : {}", fifo, e);
        return 1;
    }
    let mut sortie = match OpenOptions::new().create(true).append(true).open(journal) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Journal impossible ({}) : {}", journal, e);
            return 1;
        }
    };
    let taille = nix::pty::Winsize { ws_row: LIGNES, ws_col: COLONNES, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = match nix::pty::openpty(Some(&taille), None::<&nix::sys::termios::Termios>) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(sortie, "Pseudo-terminal impossible : {}", e);
            return 1;
        }
    };
    let ctty = crate::dans_le_path("setsid").is_some();
    let mut c = if ctty {
        let mut c = Command::new("setsid");
        c.arg("--ctty").args(cmd);
        c
    } else {
        // sans util-linux : l'outil dans son propre groupe, le Ctrl+C est transmis par signal
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..]).process_group(0);
        c
    };
    let (Ok(entree), Ok(sortie_outil)) = (pty.slave.try_clone(), pty.slave.try_clone()) else {
        let _ = writeln!(sortie, "Pseudo-terminal impossible.");
        return 1;
    };
    c.stdin(Stdio::from(entree))
        .stdout(Stdio::from(sortie_outil))
        .stderr(Stdio::from(pty.slave))
        .env("TERM", "dumb")
        .env("COLUMNS", COLONNES.to_string())
        .env("LINES", LIGNES.to_string());
    let mut enfant = match c.spawn() {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(sortie, "Impossible de lancer {} : {}", cmd[0], e);
            let _ = std::fs::remove_file(fifo);
            return 127;
        }
    };
    drop(c); // nos copies du côté outil du terminal : sinon la fin de l'outil ne se verrait pas
    let groupe = Pid::from_raw(enfant.id() as i32);

    // arrêt demandé : à l'outil de s'arrêter
    if let Ok(mut signaux) = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP]) {
        std::thread::spawn(move || {
            for s in signaux.forever() {
                if let Ok(sig) = Signal::try_from(s) {
                    let _ = killpg(groupe, sig);
                }
            }
        });
    }

    let maitre = File::from(pty.master);
    if let (Ok(mut vers_outil), Ok(mut fifo_entree)) = (maitre.try_clone(), OpenOptions::new().read(true).write(true).open(fifo)) {
        // lecture et écriture : la FIFO n'a jamais de fin de fichier
        std::thread::spawn(move || {
            let mut tampon = [0u8; 4096];
            while let Ok(n) = fifo_entree.read(&mut tampon) {
                if n == 0 {
                    continue;
                }
                let _ = vers_outil.write_all(&tampon[..n]);
                if !ctty && tampon[..n].contains(&3) {
                    let _ = killpg(groupe, Signal::SIGINT);
                }
            }
        });
    }
    let (fini, signal_fini) = mpsc::channel();
    let mut lecteur = maitre;
    std::thread::spawn(move || {
        let mut tampon = [0u8; 65536];
        // EIO : l'outil a fermé son terminal
        while let Ok(n) = lecteur.read(&mut tampon) {
            if n == 0 || sortie.write_all(&tampon[..n]).is_err() {
                break;
            }
        }
        let _ = fini.send(());
    });
    let statut = loop {
        match enfant.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => break None,
        }
    };
    let _ = signal_fini.recv_timeout(Duration::from_secs(1)); // ce qui reste à afficher
    let _ = std::fs::remove_file(fifo);
    match statut {
        Some(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
        None => 1,
    }
}
