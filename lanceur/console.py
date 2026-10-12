"""Terminal d'un web tool : lance la commande dans un pseudo-terminal (comme une fenêtre de
terminal), écrit tout ce qu'elle affiche dans son journal, et lui transmet ce qui est
saisi dans la page « Terminaux » du launcher (via une FIFO).

    python3 console.py <fifo> <journal> -- commande args...

Ce petit processus vit à côté de l'outil, indépendamment du launcher : un redémarrage
du launcher ne coupe pas le terminal. Il se termine avec l'outil, avec son code de sortie.
"""

import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios

LIGNES, COLONNES = 40, 120


def main(argv):
    if len(argv) < 4 or argv[2] != "--":
        sys.exit("usage : console.py <fifo> <journal> -- commande...")
    fifo, journal, cmd = argv[0], argv[1], argv[3:]
    try:
        os.unlink(fifo)
    except FileNotFoundError:
        pass
    os.mkfifo(fifo, 0o600)

    pid, maitre = pty.fork()
    if pid == 0:                                    # l'outil, dans le terminal
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", LIGNES, COLONNES, 0, 0))
        except OSError:
            pass
        os.environ.update(TERM="dumb", COLUMNS=str(COLONNES), LINES=str(LIGNES))
        try:
            os.execvp(cmd[0], cmd)
        except OSError as exc:
            os.write(2, ("Impossible de lancer %s : %s\n" % (cmd[0], exc)).encode())
            os._exit(127)

    def transmettre(sig, _cadre):                   # arrêt demandé : à l'outil de s'arrêter
        try:
            os.killpg(pid, sig)
        except (ProcessLookupError, PermissionError):
            pass
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, transmettre)

    entree = os.open(fifo, os.O_RDWR | os.O_NONBLOCK)  # RDWR : jamais de fin de fichier
    statut = None
    with open(journal, "ab", buffering=0) as sortie:
        while True:
            try:
                prets, _, _ = select.select([maitre, entree], [], [], 1.0)
            except InterruptedError:
                continue
            if maitre in prets:
                try:
                    donnees = os.read(maitre, 65536)
                except OSError:                     # EIO : l'outil a fermé son terminal
                    donnees = b""
                if not donnees:
                    break
                sortie.write(donnees)
            if entree in prets:
                try:
                    saisie = os.read(entree, 4096)
                    if saisie:
                        os.write(maitre, saisie)
                except OSError:
                    pass
            fini, s = os.waitpid(pid, os.WNOHANG)
            if fini == pid:
                statut = s
                try:                                # ce qui reste à afficher
                    while True:
                        donnees = os.read(maitre, 65536)
                        if not donnees:
                            break
                        sortie.write(donnees)
                except OSError:
                    pass
                break
    if statut is None:
        _, statut = os.waitpid(pid, 0)
    os.close(entree)
    try:
        os.unlink(fifo)
    except OSError:
        pass
    code = os.waitstatus_to_exitcode(statut) if hasattr(os, "waitstatus_to_exitcode") else statut >> 8
    sys.exit(code if code >= 0 else 128 - code)


if __name__ == "__main__":
    main(sys.argv[1:])
