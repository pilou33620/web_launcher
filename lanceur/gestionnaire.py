"""Cycle de vie des web tools : installer, démarrer, arrêter, vérifier et appliquer les mises à jour.

Chaque outil tourne dans sa propre session (groupe de processus) et écrit dans
journaux/<id>.log. Son PID est gardé dans etat.local.json : si le launcher redémarre
(mise à jour, plantage), il retrouve les outils déjà lancés au lieu de les relancer.
Un outil allumé au moment où le Raspberry Pi s'éteint est rallumé au démarrage suivant.
"""

import os
import re
import signal
import subprocess
import sys
import threading
import time

from . import config, depot, reseau

LINUX = sys.platform.startswith("linux")
TAILLE_JOURNAL = 2 * 1024 * 1024        # au-delà, l'ancien journal devient <id>.log.1
DELAI_DEMARRAGE = 90                    # s pour que le port réponde
RELANCES_MAX = 3                        # relances automatiques après un plantage...
FENETRE_RELANCES = 600                  # ...dans cette fenêtre (s)
DUREE_CODE = 600                        # un code d'appairage WEB·SUITE vit 10 min
CONSOLE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "console.py")


class OccupeErreur(RuntimeError):
    pass


def heure_lancement(pid):
    """Heure de lancement d'un processus (Linux), pour un outil repris ; sinon maintenant."""
    try:
        with open("/proc/%d/stat" % pid) as f:
            depart = int(f.read().rsplit(")", 1)[1].split()[19])
        with open("/proc/stat") as f:
            boot = next(int(l.split()[1]) for l in f if l.startswith("btime"))
        return boot + depart / os.sysconf("SC_CLK_TCK")
    except (OSError, ValueError, IndexError, StopIteration):
        return time.time()


def duree_allume():
    """Secondes depuis l'allumage du Pi (Linux) ; ailleurs, comme un Pi qui démarre."""
    try:
        with open("/proc/uptime") as f:
            return float(f.read().split()[0])
    except (OSError, ValueError, IndexError):
        return 0.0


class Outil:
    def __init__(self, conf, racine_outils):
        self.conf = conf
        self.id = conf["id"]
        self.port = int(conf["port"])
        self.racine_outils = racine_outils
        self.verrou = threading.Lock()      # une action à la fois
        self.action = None                  # action en cours (texte), ou None
        self.proc = None                    # Popen si lancé par ce processus
        self.pid = None
        self.lance_le = 0.0
        self.arret_demande = False
        self.voulu = False                  # doit tourner (rallumé au démarrage du Pi)
        self.maj_auto = True                # installer seul les mises à jour trouvées à chaque vérification
        self.phase = "arrete"
        self.message = ""
        self.code_sortie = None
        self.relances = []
        self.maj = {"nombre": 0, "nouveautes": [], "modifies": [], "verifie_le": None, "erreur": None}
        self.code = None                    # (code d'appairage, vu le)
        self.attend_saisie = False          # une question attend une réponse dans le terminal
        self.ctrl_c_le = 0.0                # Ctrl+C envoyé depuis le terminal de la page
        self._version = (0.0, None)

    def version(self):
        """Commit installé, relu toutes les 15 s au plus (git est lent sur un Pi)."""
        if time.time() - self._version[0] > 15:
            self._version = (time.time(), depot.version(self.dossier) if self.installe else None)
        return self._version[1]

    def oublier_version(self):
        self._version = (0.0, None)

    # -- emplacement --------------------------------------------------

    @property
    def dossier(self):
        """Premier dossier existant parmi conf['dossiers'] (casse ignorée), sinon le premier nom."""
        noms = self.conf.get("dossiers") or [self.id]
        try:
            presents = {n.lower(): n for n in os.listdir(self.racine_outils)}
        except OSError:
            presents = {}
        for nom in noms:
            if nom.lower() in presents:
                return os.path.join(self.racine_outils, presents[nom.lower()])
        return os.path.join(self.racine_outils, noms[0])

    @property
    def script(self):
        cmd = self.conf["commande"]
        return cmd[1] if len(cmd) > 1 else cmd[0]

    @property
    def installe(self):
        return os.path.isfile(os.path.join(self.dossier, self.script))

    def python(self):
        venv = os.path.join(self.dossier, ".venv", "bin", "python")
        return venv if os.path.isfile(venv) else sys.executable

    def remplir(self, morceaux):
        valeurs = {"python": self.python(), "port": str(self.port), "dossier": self.dossier}
        return [m.format(**valeurs) for m in morceaux]

    @property
    def journal(self):
        return os.path.join(config.JOURNAUX, self.id + ".log")

    @property
    def fifo(self):
        """Entrée du terminal de l'outil (lanceur/console.py)."""
        return os.path.join(config.JOURNAUX, self.id + ".entree")

    @property
    def avec_terminal(self):
        return LINUX and self.conf.get("terminal", True)

    def depots(self):
        """Le dépôt de l'outil puis ses sous-dépôts installés (WEB·SUITE : WEB_CAO...)."""
        liste = [self.dossier]
        for nom in self.conf.get("sous_depots", []):
            d = os.path.join(self.dossier, nom)
            if depot.est_depot(d):
                liste.append(d)
        return liste


class Gestionnaire:
    def __init__(self, conf, ecrire=print):
        self.conf = conf
        self.ecrire = ecrire
        self.outils = [Outil(o, conf["dossier_outils"]) for o in conf["outils"]]
        self.par_id = {o.id: o for o in self.outils}
        self.masques = list(conf.get("outils_masques", []))    # cachés (Réglages), réglages seulement
        self.ordre = conf.get("ordre") or [o.id for o in self.outils]
        self.verrou_etat = threading.Lock()
        self.launcher_maj = {"nombre": 0, "nouveautes": [], "modifies": [], "verifie_le": None, "erreur": None}
        self.launcher_action = None
        self.heures_maj = float(conf.get("verification_maj_heures", 1) or 0)
        self.prochaine_verif = None         # heure de la prochaine vérification automatique
        self.lus = {}                       # id -> position déjà relayée du journal (_relayer_connexions)
        os.makedirs(config.JOURNAUX, exist_ok=True)

    # -- état persistant ----------------------------------------------

    def sauver(self):
        with self.verrou_etat:
            config.ecrire_etat({"outils": {o.id: {"pid": o.pid, "voulu": o.voulu, "maj_auto": o.maj_auto}
                                          for o in self.outils}})

    def reprendre(self):
        """Au démarrage du launcher : retrouve les outils qui tournent, rallume ceux voulus."""
        etat = config.lire_etat().get("outils", {})
        for o in self.outils:
            e = etat.get(o.id, {})
            o.voulu = bool(e.get("voulu"))
            o.maj_auto = bool(e.get("maj_auto", True))
            pid = e.get("pid")
            if pid and self._vivant(o, pid) and self._est_a_lui(o, pid):
                o.pid, o.lance_le, o.phase = pid, heure_lancement(pid), "demarrage"
                self.ecrire("  %-15s déjà en marche (PID %d), repris." % (o.id, pid))
            elif reseau.port_ouvert(o.port):
                self._adopter_par_port(o)
            if o.pid is None and o.voulu and o.installe and not reseau.port_ouvert(o.port):
                self.ecrire("  %-15s était allumé : redémarrage." % o.id)
                self._lancer(o)
        self.sauver()

    # -- processus ----------------------------------------------------

    def _vivant(self, o, pid):
        if o.proc is not None and o.proc.pid == pid:
            code = o.proc.poll()
            if code is not None:
                o.code_sortie = code
                return False
            return True
        try:                                # un enfant d'avant un redémarrage du launcher (même PID)
            fini, statut = os.waitpid(pid, os.WNOHANG)
            if fini == pid:
                o.code_sortie = os.waitstatus_to_exitcode(statut) if hasattr(os, "waitstatus_to_exitcode") else statut
                return False
        except ChildProcessError:
            pass
        except OSError:
            pass
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        except PermissionError:
            return True
        if LINUX:
            try:
                with open("/proc/%d/stat" % pid) as f:
                    if f.read().rsplit(")", 1)[1].split()[0] == "Z":
                        return False
            except (OSError, IndexError):
                return False
        return True

    def _est_a_lui(self, o, pid):
        """Le processus tourne-t-il dans le dossier de l'outil ? (un PID peut avoir été réutilisé)"""
        if not LINUX:
            return True
        try:
            cwd = os.path.realpath(os.readlink("/proc/%d/cwd" % pid))
        except OSError:
            return False
        d = os.path.realpath(o.dossier)
        return cwd == d or cwd.startswith(d + os.sep)

    def _adopter_par_port(self, o):
        pid = reseau.pid_sur_port(o.port) if LINUX else None
        if pid and self._est_a_lui(o, pid):
            o.pid, o.lance_le, o.phase = pid, heure_lancement(pid), "en_marche"
            o.message = "Lancé hors du launcher (service systemd ?), repris."
            self.ecrire("  %-15s lancé hors du launcher (PID %d), repris." % (o.id, pid))
            return True
        return False

    def _ouvrir_journal(self, o, rotation=False):
        # Rotation seulement au démarrage : le terminal d'un outil en marche écrit dans ce fichier.
        if rotation:
            try:
                if os.path.getsize(o.journal) > TAILLE_JOURNAL:
                    os.replace(o.journal, o.journal + ".1")
            except OSError:
                pass
        return open(o.journal, "a", encoding="utf-8", errors="replace")

    def _noter(self, o, texte):
        with self._ouvrir_journal(o) as f:
            f.write("[launcher %s] %s\n" % (time.strftime("%d/%m %H:%M:%S"), texte))

    def _executer(self, o, cmd, cwd, delai=1800):
        """Commande d'installation : sortie dans le journal de l'outil. Lève RuntimeError."""
        self._noter(o, "$ " + " ".join(cmd))
        with self._ouvrir_journal(o) as f:
            try:
                r = subprocess.run(cmd, cwd=cwd, stdin=subprocess.DEVNULL, stdout=f, stderr=subprocess.STDOUT,
                                   timeout=delai, env=dict(os.environ, PYTHONUNBUFFERED="1",
                                                           GIT_TERMINAL_PROMPT="0"))
            except (OSError, subprocess.TimeoutExpired) as exc:
                raise RuntimeError("%s : %s" % (os.path.basename(cmd[0]), exc))
        if r.returncode != 0:
            raise RuntimeError("%s a échoué (code %d), voir le journal." % (" ".join(cmd[:3]), r.returncode))

    def _lancer(self, o):
        if not o.installe:
            raise RuntimeError("%s n'est pas installé." % o.conf["nom"])
        if reseau.port_ouvert(o.port):
            if self._adopter_par_port(o):
                return
            raise RuntimeError("Le port %d est déjà pris par un autre programme." % o.port)
        cmd = o.remplir(o.conf["commande"])
        self._ouvrir_journal(o, rotation=True).close()
        self._noter(o, "démarrage : " + " ".join(cmd))
        if o.avec_terminal:
            cmd = [sys.executable, CONSOLE, o.fifo, o.journal, "--"] + cmd
        journal = self._ouvrir_journal(o)
        try:
            o.proc = subprocess.Popen(cmd, cwd=o.dossier, stdin=subprocess.DEVNULL, stdout=journal,
                                      stderr=subprocess.STDOUT, start_new_session=True,
                                      env=dict(os.environ, PYTHONUNBUFFERED="1", PYTHONIOENCODING="utf-8"))
        finally:
            journal.close()
        o.pid, o.lance_le, o.code_sortie = o.proc.pid, time.time(), None
        o.arret_demande, o.code, o.attend_saisie = False, None, False
        o.phase, o.message = "demarrage", "Démarrage…"
        o.voulu = True
        self.sauver()

    def _arreter(self, o):
        if o.pid is None:
            return
        pid = o.pid
        o.arret_demande = True
        o.phase, o.message = "arret", "Arrêt…"
        self._noter(o, "arrêt demandé (PID %d)" % pid)
        self._signal(pid, signal.SIGTERM)
        fin = time.time() + float(o.conf.get("delai_arret", 10))
        while time.time() < fin and self._vivant(o, pid):
            time.sleep(0.25)
        if self._vivant(o, pid):
            self._noter(o, "ne s'arrête pas : arrêt forcé")
            self._signal(pid, signal.SIGKILL)
            time.sleep(0.5)
            self._vivant(o, pid)
        o.pid, o.proc = None, None
        o.phase, o.message = "arrete", ""
        self.sauver()

    @staticmethod
    def _signal(pid, sig):
        if sig == signal.SIGKILL and LINUX:         # l'outil derrière son terminal (console.py)
            try:
                with open("/proc/%d/task/%d/children" % (pid, pid)) as f:
                    for enfant in f.read().split():
                        try:
                            os.killpg(int(enfant), sig)
                        except (ProcessLookupError, PermissionError):
                            pass
            except OSError:
                pass
        try:
            if os.getpgid(pid) == pid:      # notre outil : toute sa session (ses sous-processus)
                os.killpg(pid, sig)
            else:
                os.kill(pid, sig)
        except (ProcessLookupError, PermissionError):
            pass

    # -- actions (en arrière-plan, une à la fois par outil) ----------

    def lancer_action(self, oid, nom):
        o = self.par_id.get(oid)
        if o is None:
            raise KeyError(oid)
        actions = {
            "demarrer": ("Démarrage", self._a_demarrer),
            "arreter": ("Arrêt", self._a_arreter),
            "redemarrer": ("Redémarrage", self._a_redemarrer),
            "verifier": ("Recherche de mises à jour", self._a_verifier),
            "mettre_a_jour": ("Mise à jour", self._a_mettre_a_jour),
            "installer": ("Installation", self._a_installer),
        }
        if nom not in actions:
            raise KeyError(nom)
        libelle, fonction = actions[nom]
        if not o.verrou.acquire(blocking=False):
            raise OccupeErreur("%s en cours" % (o.action or "Une action"))
        o.action = libelle

        def tache():
            try:
                fonction(o)
            except (RuntimeError, depot.ErreurGit, OSError) as exc:
                o.message = str(exc)
                if o.pid is None and nom in ("demarrer", "redemarrer", "installer"):
                    o.phase = "erreur"
                self._noter(o, "%s : %s" % (libelle, exc))
            finally:
                o.action = None
                o.verrou.release()
        threading.Thread(target=tache, daemon=True).start()

    def _a_demarrer(self, o):
        if o.pid is None:
            o.relances = []
            self._lancer(o)

    def _a_arreter(self, o):
        o.voulu = False
        self._arreter(o)

    def _a_redemarrer(self, o):
        self._arreter(o)
        self._lancer(o)

    def _a_verifier(self, o):
        o.message = "Recherche de mises à jour…"
        self._verifier(o)
        o.message = ""

    def _verifier(self, o):
        nombre, nouveautes, modifies = 0, [], []
        try:
            for d in o.depots():
                r = depot.verifier(d)
                nom = os.path.basename(d) if d != o.dossier else ""
                nombre += r["nombre"]
                nouveautes += [("%s : %s" % (nom, n)) if nom else n for n in r["nouveautes"]]
                modifies += [os.path.join(nom, m) if nom else m for m in r["modifies"]]
        except depot.ErreurGit as exc:
            o.maj.update(erreur=str(exc), verifie_le=time.time())
            raise
        o.maj.update(nombre=nombre, nouveautes=nouveautes[:15], modifies=modifies,
                     verifie_le=time.time(), erreur=None)

    def _a_mettre_a_jour(self, o):
        o.message = "Recherche de mises à jour…"
        self._verifier(o)
        if not o.maj["nombre"]:
            o.message = "Déjà à jour."
            return
        if o.maj["modifies"]:
            raise RuntimeError("Fichiers modifiés sur le Raspberry Pi, mise à jour bloquée : "
                               + ", ".join(o.maj["modifies"][:5]))
        etait_lance = o.pid is not None
        if etait_lance:
            self._arreter(o)
        o.phase, o.message = "maj", "Téléchargement de la nouvelle version…"
        self._noter(o, "mise à jour")
        try:
            for d in o.depots():
                changes = depot.appliquer(d)
                self._noter(o, "%s : %d fichier(s) changé(s)" % (os.path.basename(d), len(changes)))
                if "requirements.txt" in changes and o.conf.get("venv") and d == o.dossier:
                    o.message = "Mise à jour des bibliothèques Python…"
                    self._installer_venv(o)
        finally:
            o.phase = "arrete"
            o.oublier_version()
            o.maj.update(nombre=0, nouveautes=[], verifie_le=time.time(), erreur=None)
            if etait_lance:
                o.message = "Redémarrage…"
                self._lancer(o)
        if not etait_lance:
            o.message = "Mis à jour."

    def _installer_venv(self, o):
        venv = os.path.join(o.dossier, ".venv")
        if not os.path.isdir(venv):
            try:
                self._executer(o, [sys.executable, "-m", "venv", venv], o.dossier, delai=300)
            except RuntimeError:
                self._noter(o, "venv impossible (sudo apt install python3-venv) : Python du système utilisé")
                return
        if os.path.isfile(os.path.join(o.dossier, "requirements.txt")):
            self._executer(o, [os.path.join(venv, "bin", "python"), "-m", "pip", "install", "-q",
                               "-r", "requirements.txt"], o.dossier)

    def _a_installer(self, o):
        if o.installe:
            raise RuntimeError("Déjà installé dans %s." % o.dossier)
        cible = o.dossier
        if os.path.exists(cible):
            raise RuntimeError("Le dossier %s existe mais ne contient pas %s." % (cible, o.script))
        o.phase, o.message = "installation", "Téléchargement depuis GitHub…"
        self._noter(o, "installation dans %s" % cible)
        depot.cloner(o.conf["depot"], cible)
        if o.conf.get("venv"):
            o.message = "Installation des bibliothèques Python…"
            self._installer_venv(o)
        for cmd in o.conf.get("apres_clonage", []):
            o.message = "Installation des compléments…"
            self._executer(o, o.remplir(cmd), cible)
        o.oublier_version()
        o.phase, o.message = "arrete", "Installé."
        self._noter(o, "installation terminée")

    # -- launcher lui-même --------------------------------------------

    def verifier_launcher(self):
        try:
            r = depot.verifier(config.RACINE)
            self.launcher_maj.update(r, verifie_le=time.time(), erreur=None)
        except depot.ErreurGit as exc:
            self.launcher_maj.update(erreur=str(exc), verifie_le=time.time())
        return self.launcher_maj

    def mettre_a_jour_launcher(self):
        """Applique la mise à jour du launcher. Renvoie True s'il faut redémarrer."""
        changes = depot.appliquer(config.RACINE)
        self.launcher_maj.update(nombre=0, nouveautes=[], verifie_le=time.time(), erreur=None)
        self.sauver()
        return bool(changes)

    # -- surveillance -------------------------------------------------

    def surveiller(self, arret=None):
        arret = arret or threading.Event()
        threading.Thread(target=self._boucle_maj, args=(arret,), daemon=True).start()
        while not arret.wait(2):
            for o in self.outils:
                try:
                    self._actualiser(o)
                    self._relayer_connexions(o)
                except Exception as exc:  # la surveillance ne doit jamais s'arrêter
                    self.ecrire("  surveillance %s : %s" % (o.id, exc))

    def _relayer_connexions(self, o):
        """Recopie dans la sortie du launcher les lignes [connexion] du journal de l'outil :
        qui utilise quel outil, depuis quelle IP."""
        try:
            with open(o.journal, "rb") as f:
                taille = f.seek(0, os.SEEK_END)
                lu = self.lus.setdefault(o.id, taille)  # ce qui précède le démarrage est déjà vu
                if taille < lu:
                    lu = 0                              # journal renouvelé (.log.1)
                f.seek(lu)
                bloc = f.read()
        except OSError:
            self.lus.setdefault(o.id, 0)                # pas encore de journal : tout sera neuf
            return
        fin = bloc.rfind(b"\n") + 1                     # les lignes complètes seulement
        self.lus[o.id] = lu + fin
        for ligne in bloc[:fin].decode("utf-8", "replace").splitlines():
            if "[connexion]" in ligne:
                self.ecrire(ligne.rstrip())

    def _actualiser(self, o):
        if o.action and o.phase in ("arret", "maj", "installation"):
            return
        if o.pid is not None:
            if not self._vivant(o, o.pid):
                code = o.code_sortie
                o.pid, o.proc = None, None
                self.sauver()
                o.attend_saisie = False
                # 0, ou juste après un Ctrl+C : arrêté volontairement, pas de relance
                if o.arret_demande or code == 0 or time.time() - o.ctrl_c_le < 15:
                    if not o.arret_demande:
                        o.voulu = False
                        self.sauver()
                        self._noter(o, "s'est arrêté normalement")
                    o.phase, o.message = "arrete", ""
                    return
                self._noter(o, "s'est arrêté tout seul (code %s)" % code)
                o.phase = "erreur"
                o.message = "S'est arrêté (code %s), voir le journal." % code
                maintenant = time.time()
                o.relances = [t for t in o.relances if maintenant - t < FENETRE_RELANCES]
                if o.voulu and len(o.relances) < RELANCES_MAX and o.verrou.acquire(blocking=False):
                    try:
                        o.relances.append(maintenant)
                        self._noter(o, "relance automatique (%d/%d)" % (len(o.relances), RELANCES_MAX))
                        self._lancer(o)
                    except RuntimeError as exc:
                        o.message = str(exc)
                    finally:
                        o.verrou.release()
                return
            if reseau.port_ouvert(o.port):
                if o.phase != "en_marche":
                    o.phase = "en_marche"
                    if o.message in ("Démarrage…", "Redémarrage…"):
                        o.message = ""
            elif o.phase == "en_marche" or time.time() - o.lance_le > DELAI_DEMARRAGE:
                o.phase = "demarrage"
                o.message = "Ne répond pas sur le port %d, voir le journal." % o.port
            self._lire_code(o)
            o.attend_saisie = o.avec_terminal and self._question_en_attente(o)
        elif not o.action:
            if reseau.port_ouvert(o.port):
                if not self._adopter_par_port(o):
                    o.phase = "externe"
                    o.message = "Port %d occupé par un programme lancé hors du launcher." % o.port
            elif o.phase in ("en_marche", "demarrage", "externe"):
                o.phase, o.message = "arrete", ""

    def _question_en_attente(self, o):
        """La dernière ligne du terminal est une question restée sans réponse depuis 2 s :
        l'outil attend une saisie (ex. WEB·SUITE : « Se connecter maintenant ? [O/n] »)."""
        try:
            if time.time() - os.path.getmtime(o.journal) < 2:
                return False
            with open(o.journal, "rb") as f:
                f.seek(0, os.SEEK_END)
                f.seek(max(0, f.tell() - 300))
                fin = f.read().decode("utf-8", "replace")
        except OSError:
            return False
        derniere = fin.rsplit("\n", 1)[-1].strip()
        return bool(derniere) and (derniere.endswith((":", "?", "]", ">")) or "[o/n" in derniere.lower())

    def envoyer(self, oid, texte):
        """Saisie au clavier dans le terminal de l'outil. Lève KeyError, RuntimeError."""
        o = self.par_id[oid]
        if not o.avec_terminal or o.pid is None:
            raise RuntimeError("%s n'a pas de terminal ouvert." % o.conf["nom"])
        try:
            fd = os.open(o.fifo, os.O_WRONLY | os.O_NONBLOCK)
        except OSError:
            raise RuntimeError("Terminal indisponible (outil lancé hors du launcher ou avant son terminal).")
        try:
            os.write(fd, texte.encode("utf-8"))
        finally:
            os.close(fd)
        if "\x03" in texte:
            o.ctrl_c_le = time.time()
        o.attend_saisie = False

    def _lire_code(self, o):
        motif = o.conf.get("code_appairage")
        if not motif:
            return
        try:
            with open(o.journal, "rb") as f:
                f.seek(0, os.SEEK_END)
                f.seek(max(0, f.tell() - 32768))
                fin = f.read().decode("utf-8", "replace")
        except OSError:
            return
        # seulement depuis le dernier démarrage de cet outil
        debut = fin.rfind("] démarrage : ")
        if debut >= 0:
            fin = fin[debut:]
        trouves = re.findall(motif, fin)
        if trouves and (o.code is None or o.code[0] != trouves[-1]):
            o.code = (trouves[-1], time.time())

    def regler(self, oid, port=None, affiche=None, fichier=None):
        """Port et affichage d'un outil arrêté (page Réglages), gardés dans config.local.json.
        Lève KeyError, OccupeErreur, ValueError (port refusé)."""
        o = self.par_id.get(oid)
        conf = o.conf if o else next((c for c in self.masques if c["id"] == oid), None)
        if conf is None:
            raise KeyError(oid)
        if o and not o.verrou.acquire(blocking=False):
            raise OccupeErreur("%s est occupé, réessayer dans un instant." % conf["nom"])
        try:
            if o and (o.action or o.pid or o.phase == "externe"):
                raise OccupeErreur("Arrêter %s%s avant de changer ses réglages." % (conf["nom"], conf.get("suffixe", "")))
            champs = {}
            if port is not None:
                try:
                    port = int(port)
                except (TypeError, ValueError):
                    raise ValueError("Port : un nombre, par exemple 8080.")
            if port is not None and port != int(conf["port"]):
                if not 1024 <= port <= 65535:
                    raise ValueError("Port : entre 1024 et 65535.")
                if port in (self.conf.get("port"), self.conf.get("port_secours")):
                    raise ValueError("Port %d : c'est celui du launcher." % port)
                autres = [x.conf for x in self.outils] + self.masques
                pris = next((x for x in autres if x["id"] != oid and int(x["port"]) == port), None)
                if pris:
                    raise ValueError("Port %d : déjà celui de %s%s." % (port, pris["nom"], pris.get("suffixe", "")))
                if reseau.port_ouvert(port):
                    raise ValueError("Port %d : déjà utilisé par un autre programme." % port)
                champs["port"] = port
            if affiche is not None and bool(affiche) == bool(conf.get("masque")):
                champs["masque"] = None if affiche else True
            if not champs:
                return
            config.regler_outil(oid, fichier or config.LOCAL, **champs)
            if "port" in champs:
                conf["port"] = port
                if o:
                    o.port = port
                    o.message = ""
        finally:
            if o:
                o.verrou.release()
        if "masque" in champs:
            if affiche:
                conf.pop("masque", None)
                self.masques.remove(conf)
                rang = {i: n for n, i in enumerate(self.ordre)}
                outils = sorted(self.outils + [Outil(conf, self.conf["dossier_outils"])],
                                key=lambda x: rang.get(x.id, len(rang)))
            else:
                conf["masque"] = True
                self.masques.append(conf)
                outils = [x for x in self.outils if x is not o]
            self.outils, self.par_id = outils, {x.id: x for x in outils}
            self.sauver()

    def regler_maj_auto(self, oid, valeur):
        o = self.par_id[oid]
        o.maj_auto = bool(valeur)
        self._noter(o, "mise à jour automatique %s" % ("activée" if o.maj_auto else "désactivée"))
        self.sauver()

    def verifier_tous(self):
        """Vérification automatique : le launcher et chaque outil installé ; les outils en
        « mise à jour auto » installent aussitôt ce qui est trouvé (et redémarrent)."""
        self.verifier_launcher()
        for o in self.outils:
            if not o.installe or not o.verrou.acquire(blocking=False):
                continue                    # occupé : ce sera pour la prochaine fois
            o.action = "Recherche de mises à jour"
            try:
                self._verifier(o)
                if o.maj_auto and o.maj["nombre"] and not o.maj["modifies"]:
                    o.action = "Mise à jour automatique"
                    self._noter(o, "mise à jour automatique : %d nouveauté(s)" % o.maj["nombre"])
                    self._a_mettre_a_jour(o)
                    o.message = "Mis à jour automatiquement le %s." % time.strftime("%d/%m à %H:%M")
            except (depot.ErreurGit, RuntimeError, OSError) as exc:
                o.message = "Mise à jour automatique : %s" % exc if o.maj_auto else o.message
                self._noter(o, "vérification automatique : %s" % exc)
            finally:
                o.action = None
                o.verrou.release()

    def _boucle_maj(self, arret):
        """Vérification à chaque démarrage du launcher, puis toutes les N heures (si réglé)."""
        delai = 30 if duree_allume() < 180 else 5   # Pi qui démarre : laisser le réseau se connecter
        self.prochaine_verif = time.time() + delai
        if arret.wait(delai):
            return
        while True:
            self.verifier_tous()
            if self.heures_maj <= 0:
                self.prochaine_verif = None
                return
            self.prochaine_verif = time.time() + self.heures_maj * 3600
            if arret.wait(self.heures_maj * 3600):
                return

    # -- vue ----------------------------------------------------------

    def etat(self):
        liste = []
        for o in self.outils:
            code = None
            if o.code and o.pid and time.time() - o.code[1] < DUREE_CODE:
                code = {"code": o.code[0], "age": int(time.time() - o.code[1])}
            liste.append({
                "id": o.id, "nom": o.conf["nom"], "suffixe": o.conf.get("suffixe", ""),
                "description": o.conf.get("description", ""), "accent": o.conf.get("accent", "#888"),
                "icone": o.conf.get("icone", ""), "depot": o.conf["depot"],
                "site": o.conf["depot"][:-4] if o.conf["depot"].endswith(".git") else o.conf["depot"],
                "port": o.port, "dossier": o.dossier, "installe": o.installe,
                "phase": o.phase, "action": o.action, "message": o.message, "pid": o.pid,
                "voulu": o.voulu, "maj_auto": o.maj_auto, "depuis": int(o.lance_le) if o.pid else None,
                "version": o.version(),
                "maj": o.maj, "code_appairage": code,
                "terminal": o.avec_terminal, "attend_saisie": o.attend_saisie,
                "sous_depots": [os.path.basename(d) for d in o.depots()[1:]] if o.installe else [],
            })
        return liste
