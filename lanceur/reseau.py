"""Réseau : adresse du Raspberry Pi, ports, et le nom « launcher.local » (mDNS, via Avahi)."""

import os
import shutil
import socket
import subprocess
import threading
import time


def ip_locale():
    """Adresse IPv4 du Raspberry Pi sur le réseau local (aucun paquet n'est envoyé)."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("10.255.255.255", 1))
        return s.getsockname()[0]
    except OSError:
        return None
    finally:
        s.close()


def nom_machine():
    return socket.gethostname().split(".")[0].lower()


def port_ouvert(port, hote="127.0.0.1", delai=0.4):
    try:
        with socket.create_connection((hote, port), timeout=delai):
            return True
    except OSError:
        return False


def _inodes_ecoute(port):
    """Inodes des sockets TCP en écoute sur ce port (Linux, /proc/net/tcp*)."""
    inodes = set()
    for fichier in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            with open(fichier) as f:
                next(f, None)
                for ligne in f:
                    champs = ligne.split()
                    if len(champs) > 9 and champs[3] == "0A" and int(champs[1].rsplit(":", 1)[1], 16) == port:
                        inodes.add(champs[9])
        except OSError:
            pass
    return inodes


def pid_sur_port(port):
    """PID du programme qui écoute sur ce port, s'il appartient au même utilisateur (sinon None)."""
    inodes = _inodes_ecoute(port)
    if not inodes:
        return None
    cibles = {"socket:[%s]" % i for i in inodes}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        dossier_fd = "/proc/%s/fd" % pid
        try:
            for fd in os.listdir(dossier_fd):
                try:
                    if os.readlink(os.path.join(dossier_fd, fd)) in cibles:
                        return int(pid)
                except OSError:
                    continue
        except OSError:
            continue
    return None


class AliasMdns:
    """Annonce « <nom>.local » sur le réseau local avec avahi-publish, et suit les
    changements d'adresse IP (box redémarrée, Wi-Fi). Rien ne se passe si Avahi
    n'est pas installé, ou si la machine porte déjà ce nom (Avahi l'annonce alors)."""

    def __init__(self, nom, journal=print):
        self.nom = nom
        self.journal = journal
        self.proc = None
        self.ip = None
        self.etat = "inactif"

    def demarrer(self):
        if not self.nom:
            self.etat = "désactivé"
            return
        if nom_machine() == self.nom.lower():
            self.etat = "nom de la machine"
            return
        if not shutil.which("avahi-publish"):
            self.etat = "avahi-utils absent"
            self.journal("  mDNS : avahi-publish introuvable (sudo apt install avahi-utils), "
                         "%s.local ne sera pas annoncé." % self.nom)
            return
        threading.Thread(target=self._boucle, daemon=True).start()

    def _boucle(self):
        while True:
            ip = ip_locale()
            vivant = self.proc is not None and self.proc.poll() is None
            if ip and (ip != self.ip or not vivant):
                self._arreter()
                try:
                    self.proc = subprocess.Popen(
                        ["avahi-publish", "-a", "-R", "%s.local" % self.nom, ip],
                        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    self.ip = ip
                    self.etat = "annoncé"
                except OSError as exc:
                    self.etat = "erreur : %s" % exc
            time.sleep(30)

    def _arreter(self):
        if self.proc is not None and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        self.proc = None

    def arreter(self):
        self._arreter()
