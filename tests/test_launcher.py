"""Tests du launcher avec un faux web tool (dépôt git local, aucun accès réseau).

    python3 -m unittest discover tests
"""
import datetime
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lanceur import andy, claude, compte, config, reseau  # noqa: E402
from lanceur.gestionnaire import Gestionnaire, OccupeErreur  # noqa: E402

SERVEUR = """import http.server, sys
VERSION = %r
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(VERSION.encode())
    def log_message(self, *a): pass
print("pret", flush=True)
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
"""


def port_libre():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def git(dossier, *args):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *args], cwd=dossier,
                   check=True, capture_output=True)


def attendre(condition, delai=20):
    fin = time.time() + delai
    while time.time() < fin:
        if condition():
            return True
        time.sleep(0.2)
    return False


class FauxOutil(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        # dépôt « GitHub » : un clone de travail qui pousse vers un dépôt nu
        self.origine = os.path.join(self.tmp, "origine.git")
        self.travail = os.path.join(self.tmp, "travail")
        subprocess.run(["git", "init", "-q", "--bare", "-b", "main", self.origine], check=True)
        subprocess.run(["git", "clone", "-q", self.origine, self.travail], check=True, capture_output=True)
        self.publier("v1")
        self.documents = os.path.join(self.tmp, "Documents")
        os.makedirs(self.documents)
        self.port = port_libre()
        self._journaux = config.JOURNAUX
        self._etat = config.ETAT
        config.JOURNAUX = os.path.join(self.tmp, "journaux")
        config.ETAT = os.path.join(self.tmp, "etat.json")
        self.g = Gestionnaire({"dossier_outils": self.documents, "verification_maj_heures": 0, "outils": [{
            "id": "faux", "nom": "Faux", "depot": self.origine, "dossiers": ["faux", "FAUX_ALT"],
            "port": self.port, "commande": ["{python}", "serveur.py", "{port}"], "delai_arret": 5,
        }]}, ecrire=lambda t: None)
        self.o = self.g.par_id["faux"]
        self.addCleanup(self.nettoyer)

    def nettoyer(self):
        if self.o.pid:
            self.g._arreter(self.o)
        config.JOURNAUX, config.ETAT = self._journaux, self._etat

    def publier(self, version):
        with open(os.path.join(self.travail, "serveur.py"), "w") as f:
            f.write(SERVEUR % version)
        git(self.travail, "add", "-A")
        git(self.travail, "commit", "-q", "-m", "version " + version)
        git(self.travail, "push", "-q", "origin", "HEAD:main")

    def action(self, nom):
        self.g.lancer_action("faux", nom)
        self.assertTrue(attendre(lambda: self.o.action is None), "action %s trop longue" % nom)
        for _ in range(3):
            self.g._actualiser(self.o)

    def page(self):
        import urllib.request
        return urllib.request.urlopen("http://127.0.0.1:%d/" % self.port, timeout=3).read().decode()

    def test_cycle_complet(self):
        self.assertFalse(self.o.installe)
        self.action("installer")
        self.assertTrue(self.o.installe, self.o.message)
        self.assertEqual(self.o.version()["sujet"], "version v1")

        self.action("demarrer")
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        self.g._actualiser(self.o)
        self.assertEqual(self.o.phase, "en_marche")
        self.assertEqual(self.page(), "v1")
        self.assertTrue(config.lire_etat()["outils"]["faux"]["voulu"])

        # une nouvelle version sur « GitHub » : vue, puis appliquée pendant que l'outil tourne
        self.publier("v2")
        self.action("verifier")
        self.assertEqual(self.o.maj["nombre"], 1)
        self.assertEqual(self.o.maj["nouveautes"], ["version v2"])
        ancien_pid = self.o.pid
        self.action("mettre_a_jour")
        self.assertNotEqual(self.o.pid, ancien_pid)
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        self.assertEqual(self.page(), "v2")
        self.assertEqual(self.o.maj["nombre"], 0)

        # une modification locale bloque la mise à jour, sans rien écraser
        self.publier("v3")
        with open(os.path.join(self.o.dossier, "serveur.py"), "a") as f:
            f.write("# modifié sur le Pi\n")
        self.action("mettre_a_jour")
        self.assertIn("modifiés", self.o.message)
        self.assertEqual(self.page(), "v2")

        self.action("arreter")
        self.assertIsNone(self.o.pid)
        self.assertFalse(reseau.port_ouvert(self.port))
        self.assertFalse(config.lire_etat()["outils"]["faux"]["voulu"])

    def test_relais_des_connexions(self):
        lignes = []
        self.g.ecrire = lignes.append
        os.makedirs(config.JOURNAUX, exist_ok=True)
        with open(self.o.journal, "w") as f:
            f.write("  [connexion] avant le launcher\n")
        self.g._relayer_connexions(self.o)                 # l'ancien contenu n'est pas relayé
        with open(self.o.journal, "a") as f:
            f.write("bavardage\r\n  [connexion] 10:00:00  Faux  192.168.1.20  ->  1 appareil\r\n  [connexion] coup")
        self.g._relayer_connexions(self.o)
        self.assertEqual(lignes, ["  [connexion] 10:00:00  Faux  192.168.1.20  ->  1 appareil"])
        with open(self.o.journal, "a") as f:
            f.write("ée\n")
        self.g._relayer_connexions(self.o)                 # la ligne coupée arrive entière
        self.assertEqual(lignes[-1], "  [connexion] coupée")
        with open(self.o.journal, "w") as f:               # journal renouvelé au démarrage
            f.write("  [connexion] neuf\n")
        self.g._relayer_connexions(self.o)
        self.assertEqual(lignes[-1], "  [connexion] neuf")

    def test_mise_a_jour_automatique(self):
        self.g.verifier_launcher = lambda: None         # pas de GitHub pour le launcher lui-même
        self.action("installer")
        self.action("demarrer")
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        self.publier("v2")
        self.assertTrue(self.o.maj_auto)                # activée par défaut
        self.g.regler_maj_auto("faux", False)
        self.g.verifier_tous()                          # sans « MAJ auto » : vu, pas installé
        self.assertEqual(self.o.maj["nombre"], 1)
        self.assertEqual(self.page(), "v1")
        self.g.regler_maj_auto("faux", True)
        self.assertTrue(config.lire_etat()["outils"]["faux"]["maj_auto"])
        self.g.verifier_tous()
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        self.assertEqual(self.page(), "v2")
        self.assertEqual(self.o.maj["nombre"], 0)
        self.assertIn("automatiquement", self.o.message)

    def test_reprise_et_relance(self):
        self.action("installer")
        self.action("demarrer")
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        pid = self.o.pid

        # un nouveau launcher (après mise à jour ou plantage) retrouve l'outil sans le relancer
        g2 = Gestionnaire(self.g.conf, ecrire=lambda t: None)
        g2.reprendre()
        o2 = g2.par_id["faux"]
        self.assertEqual(o2.pid, pid)

        # plantage : relance automatique, l'outil était voulu allumé
        os.kill(pid, 9)
        self.assertTrue(attendre(lambda: (self.g._actualiser(self.o), self.o.pid not in (None, pid))[1]))
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        o2.pid = None                   # g2 n'est plus utilisé

    def test_dossier_alternatif_et_occupe(self):
        subprocess.run(["git", "clone", "-q", self.origine, os.path.join(self.documents, "faux_alt")],
                       check=True, capture_output=True)
        self.assertTrue(self.o.installe)
        self.assertTrue(self.o.dossier.endswith("faux_alt"))
        self.o.verrou.acquire()
        self.o.action = "Test"
        with self.assertRaises(OccupeErreur):
            self.g.lancer_action("faux", "demarrer")
        self.o.verrou.release()
        self.o.action = None


    def test_terminal_question_et_reponse(self):
        """L'outil tourne dans un terminal : on lit sa question et on lui répond depuis la page."""
        with open(os.path.join(self.travail, "serveur.py"), "w") as f:
            f.write("import sys\nnom = input('Votre nom ? ')\nprint('Bonjour', nom, sys.stdin.isatty())\n"
                    + SERVEUR % "v1")
        git(self.travail, "commit", "-q", "-am", "question")
        git(self.travail, "push", "-q", "origin", "HEAD:main")
        self.action("installer")
        self.action("demarrer")
        self.assertTrue(attendre(lambda: (self.g._actualiser(self.o), self.o.attend_saisie)[1], 10),
                        "la question n'est pas vue")
        self.assertFalse(reseau.port_ouvert(self.port))
        self.g.envoyer("faux", "Pi\r")
        self.assertTrue(attendre(lambda: reseau.port_ouvert(self.port)))
        from lanceur.serveur import lire_depuis
        position, texte = lire_depuis(self.o.journal, 0)
        self.assertIn("Bonjour Pi True", texte)
        self.assertEqual(position, os.path.getsize(self.o.journal))
        # Ctrl+C : l'outil s'arrête normalement, il n'est pas relancé
        self.g.envoyer("faux", "\x03")
        self.assertTrue(attendre(lambda: (self.g._actualiser(self.o), self.o.pid is None)[1]))
        self.assertEqual(self.o.phase, "arrete")
        self.assertFalse(self.o.voulu)


class Compte(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        self.fichier, self.sessions = os.path.join(tmp, "compte.json"), os.path.join(tmp, "sessions.json")
        saisies = iter(["pilou", "court", "correct horse", "autre chose", "correct horse", "correct horse"])
        compte.creer_compte(self.fichier, self.sessions, saisir=lambda _: next(saisies),
                            secret=lambda _: next(saisies), ecrire=lambda _: None)
        self.g = compte.Gardien(self.fichier, self.sessions)

    def test_mot_de_passe_jamais_en_clair(self):
        with open(self.fichier) as f:
            contenu = f.read()
        self.assertNotIn("correct horse", contenu)
        self.assertEqual(os.stat(self.fichier).st_mode & 0o777, 0o600)

    def test_connexion_sessions_et_blocage(self):
        jeton, erreur = self.g.verifier("Pilou", "correct horse", "10.0.0.1")
        self.assertIsNone(erreur)
        self.assertTrue(self.g.session_valide(jeton))
        with open(self.sessions) as f:
            self.assertNotIn(jeton, f.read())          # seule l'empreinte est gardée
        self.assertTrue(compte.Gardien(self.fichier, self.sessions).session_valide(jeton))
        self.g.fermer(jeton)
        self.assertFalse(self.g.session_valide(jeton))
        for _ in range(compte.ESSAIS):
            self.assertIsNone(self.g.verifier("pilou", "faux", "10.0.0.2")[0])
        # adresse bloquée, même avec le bon mot de passe ; les autres adresses non
        self.assertIsNone(self.g.verifier("pilou", "correct horse", "10.0.0.2")[0])
        self.assertIsNotNone(self.g.verifier("pilou", "correct horse", "10.0.0.3")[0])
        self.assertIsNone(self.g.verifier("autre", "correct horse", "10.0.0.4")[0])

    def test_plusieurs_comptes(self):
        self.assertIn("existe", self.g.ajouter("PILOU", "un autre mdp"))
        self.assertIn("court", self.g.ajouter("marie", "court"))
        self.assertIsNone(self.g.ajouter("marie", "mot de passe marie"))
        self.assertEqual([c["login"] for c in self.g.comptes()], ["pilou", "marie"])
        j_pilou = self.g.verifier("pilou", "correct horse", "10.0.0.1")[0]
        j_marie = self.g.verifier("Marie", "mot de passe marie", "10.0.0.1")[0]
        j_marie2 = self.g.verifier("marie", "mot de passe marie", "10.0.0.5")[0]
        self.assertEqual(self.g.utilisateur(j_marie), "marie")
        self.assertIsNone(self.g.verifier("marie", "correct horse", "10.0.0.1")[0])
        # nouveau mot de passe : les autres appareils de marie déconnectés, pas celui qui le change
        self.assertIsNone(self.g.changer_mdp("marie", "nouveau mdp marie", garder=j_marie))
        self.assertTrue(self.g.session_valide(j_marie))
        self.assertFalse(self.g.session_valide(j_marie2))
        self.assertTrue(self.g.session_valide(j_pilou))
        self.assertIsNotNone(self.g.verifier("marie", "nouveau mdp marie", "10.0.0.1")[0])
        # suppression : ses sessions tombent ; le dernier compte reste
        self.assertIsNone(self.g.supprimer("marie"))
        self.assertFalse(self.g.session_valide(j_marie))
        self.assertTrue(self.g.session_valide(j_pilou))
        self.assertIn("dernier", self.g.supprimer("pilou"))
        # en console : un identifiant existant change son mot de passe, un nouveau est ajouté
        saisies = iter(["paul", "mdp de paul!", "mdp de paul!"])
        self.assertTrue(compte.creer_compte(self.fichier, self.sessions, saisir=lambda _: next(saisies),
                                            secret=lambda _: next(saisies), ecrire=lambda _: None))
        self.assertEqual([c["login"] for c in self.g.comptes()], ["pilou", "paul"])
        self.assertTrue(self.g.session_valide(j_pilou))

    def test_ancien_fichier_a_un_compte(self):
        jeton = self.g.verifier("pilou", "correct horse", "10.0.0.1")[0]
        with open(self.fichier) as f:
            ancien = json.load(f)["comptes"][0]
        with open(self.fichier, "w") as f:
            json.dump(ancien, f)                        # format d'avant : un seul compte
        with open(self.sessions, "w") as f:
            json.dump({compte.Gardien._empreinte(jeton): time.time() + 60}, f)
        g = compte.Gardien(self.fichier, self.sessions)
        self.assertEqual(g.utilisateur(jeton), "pilou")
        self.assertIsNotNone(g.verifier("pilou", "correct horse", "10.0.0.1")[0])
        self.assertIsNone(g.ajouter("marie", "mot de passe marie"))
        self.assertEqual([c["login"] for c in g.comptes()], ["pilou", "marie"])

    def test_serveur_protege(self):
        import threading
        import urllib.error
        import urllib.request
        from lanceur import serveur
        serveur.Lanceur.gardien = self.g
        serveur.Lanceur.gestionnaire = Gestionnaire({"dossier_outils": "/nulle-part", "outils": []},
                                                    ecrire=lambda t: None)
        serveur.Lanceur.infos = {"port": 0}
        httpd = serveur.ThreadingHTTPServer(("127.0.0.1", 0), serveur.Lanceur)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.shutdown)
        base = "http://127.0.0.1:%d" % httpd.server_address[1]

        class SansRedirection(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *a):
                return None
        ouvrir = urllib.request.build_opener(SansRedirection).open

        def statut(chemin, donnees=None, cookie=None):
            req = urllib.request.Request(base + chemin, data=donnees, headers={"X-Launcher": "1"})
            if cookie:
                req.add_header("Cookie", cookie)
            try:
                r = ouvrir(req, timeout=10)
                return r.status, r.headers
            except urllib.error.HTTPError as e:
                return e.code, e.headers
        self.assertEqual(statut("/")[0], 302)
        self.assertEqual(statut("/console.html")[0], 302)
        self.assertEqual(statut("/api/etat")[0], 401)
        self.assertEqual(statut("/api/journal/x")[0], 401)
        self.assertEqual(statut("/api/verifier_tout", b"")[0], 401)
        self.assertEqual(statut("/connexion.html")[0], 200)
        self.assertEqual(statut("/api/connexion", b'{"login":"pilou","mot_de_passe":"non"}')[0], 401)
        code, entetes = statut("/api/connexion", b'{"login":"pilou","mot_de_passe":"correct horse"}')
        self.assertEqual(code, 200)
        biscuit = entetes["Set-Cookie"]
        self.assertIn("HttpOnly", biscuit)
        self.assertIn("SameSite=Strict", biscuit)
        cookie = biscuit.split(";")[0]
        self.assertEqual(statut("/api/etat", cookie=cookie)[0], 200)
        self.assertEqual(statut("/", cookie=cookie)[0], 200)
        statut("/api/deconnexion", b"", cookie=cookie)
        self.assertEqual(statut("/api/etat", cookie=cookie)[0], 401)
        # sans compte : ce poste entre, un autre appareil est refusé
        serveur.Lanceur.gardien = compte.Gardien(self.fichier + ".absent", self.sessions)
        self.assertEqual(statut("/api/etat")[0], 200)
        self.addCleanup(setattr, serveur.Lanceur, "ce_poste", serveur.Lanceur.ce_poste)
        serveur.Lanceur.ce_poste = lambda self: False
        self.assertEqual(statut("/api/etat")[0], 401)
        self.assertEqual(statut("/api/comptes/ajouter", b'{"login":"x","mot_de_passe":"12345678"}')[0], 401)
        self.assertEqual(statut("/")[0], 302)


class Configuration(unittest.TestCase):
    def test_config_locale(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        cat, loc = os.path.join(tmp, "o.json"), os.path.join(tmp, "l.json")
        with open(cat, "w") as f:
            json.dump({"port": 80, "outils": [{"id": "a", "port": 1}, {"id": "b", "port": 2}]}, f)
        with open(loc, "w") as f:
            json.dump({"port": 8090, "dossier_outils": "~/x",
                       "outils": [{"id": "a", "port": 9}, {"id": "b", "masque": True}, {"id": "c", "port": 3}]}, f)
        conf = config.charger(cat, loc)
        self.assertEqual(conf["port"], 8090)
        self.assertEqual([(o["id"], o["port"]) for o in conf["outils"]], [("a", 9), ("c", 3)])
        self.assertEqual([o["id"] for o in conf["outils_masques"]], ["b"])
        self.assertEqual(conf["dossier_outils"], os.path.expanduser("~/x"))

    def test_reglages_depuis_la_page(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        loc = os.path.join(tmp, "l.json")
        with open(loc, "w") as f:
            json.dump({"verification_maj_heures": 12}, f)
        etat, config.ETAT = config.ETAT, os.path.join(tmp, "etat.json")   # ne pas écraser l'état réel du Pi
        self.addCleanup(setattr, config, "ETAT", etat)
        p1, p2 = port_libre(), port_libre()
        conf = {"dossier_outils": tmp, "port": 80, "port_secours": 8090, "ordre": ["a", "b"],
                "outils": [{"id": "a", "nom": "A", "port": p1, "depot": "x"},
                           {"id": "b", "nom": "B", "port": p2, "depot": "x"}]}
        g = Gestionnaire(conf, ecrire=lambda t: None)
        for port in ("abc", 80, 22, p2):
            with self.assertRaises(ValueError):
                g.regler("a", port=port, fichier=loc)
        nouveau = port_libre()
        g.regler("a", port=nouveau, fichier=loc)
        self.assertEqual(g.par_id["a"].port, nouveau)
        g.regler("a", affiche=False, fichier=loc)
        self.assertEqual([o.id for o in g.outils], ["b"])
        self.assertEqual(config.charger(os.path.join(tmp, "absent.json"), loc)["verification_maj_heures"], 12)
        with open(loc) as f:
            self.assertEqual(json.load(f)["outils"], [{"id": "a", "port": nouveau, "masque": True}])
        g.regler("a", affiche=True, fichier=loc)
        self.assertEqual([o.id for o in g.outils], ["a", "b"])
        g.par_id["b"].pid = os.getpid()                   # en marche : réglages refusés
        with self.assertRaises(OccupeErreur):
            g.regler("b", port=port_libre(), fichier=loc)
        with self.assertRaises(KeyError):
            g.regler("z", port=9000, fichier=loc)

    def test_catalogue_livre(self):
        conf = config.charger(config.CATALOGUE, os.devnull + ".absent")
        ports = [o["port"] for o in conf["outils"]]
        self.assertEqual(len(ports), len(set(ports)))
        for o in conf["outils"]:
            self.assertIn("{port}", o["commande"])


class Temp(unittest.TestCase):
    def setUp(self):
        import threading
        from lanceur import serveur
        self.serveur = serveur
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.temp = os.path.join(self.tmp, "temp")
        os.makedirs(os.path.join(self.temp, "sous"))
        os.makedirs(os.path.join(self.temp, ".cache"))
        fichiers = {"a.png": b"\x89PNG\0", "notes.txt": "été".encode(), "sous/rapport.md": b"# r", "page.html": b"<script>",
                    "image.SVG": b"<svg/>", "doc.pdf": b"%PDF", "bin é.dat": b"a\0b", ".cache/x.txt": b"x", ".cache.txt": b"x"}
        for nom, contenu in fichiers.items():
            with open(os.path.join(self.temp, nom), "wb") as f:
                f.write(contenu)
        with open(os.path.join(self.tmp, "secret.txt"), "w") as f:
            f.write("secret")
        os.symlink(os.path.join(self.tmp, "secret.txt"), os.path.join(self.temp, "lien.txt"))
        os.symlink(self.tmp, os.path.join(self.temp, "lien_dossier"))
        os.mkfifo(os.path.join(self.temp, "tube"))
        with open(os.fsencode(self.temp) + b"/mauvais\xff.txt", "w") as f:
            f.write("x")
        self.addCleanup(setattr, serveur.Lanceur, "gardien", serveur.Lanceur.gardien)
        self.addCleanup(setattr, serveur.Lanceur, "infos", serveur.Lanceur.infos)
        self.addCleanup(setattr, serveur.Lanceur, "ce_poste", serveur.Lanceur.ce_poste)
        serveur.Lanceur.gardien = compte.Gardien(os.path.join(self.tmp, "c.json"), os.path.join(self.tmp, "s.json"))
        serveur.Lanceur.infos = {"port": 0, "dossier_outils": self.tmp}
        httpd = serveur.ThreadingHTTPServer(("127.0.0.1", 0), serveur.Lanceur)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        self.base = "http://127.0.0.1:%d" % httpd.server_address[1]

    def demander(self, chemin):
        import urllib.error
        import urllib.request
        try:
            r = urllib.request.urlopen(self.base + chemin, timeout=5)
        except urllib.error.HTTPError as e:
            r = e
        return r.status, r.headers, r.read()

    def fichier(self, chemin, suite=""):
        from urllib.parse import quote
        return self.demander("/api/temp/fichier?chemin=" + quote(chemin, safe="") + suite)

    def test_liste(self):
        code, _, corps = self.demander("/api/temp")
        d = json.loads(corps)
        self.assertEqual(code, 200)
        self.assertEqual(d["dossier"], self.temp)
        genres = {f["chemin"]: f["genre"] for f in d["fichiers"]}
        self.assertEqual(genres, {"a.png": "image", "notes.txt": "texte", "sous/rapport.md": "texte", "page.html": "html",
                                  "image.SVG": "image", "doc.pdf": "pdf", "bin é.dat": "autre"})
        os.utime(os.path.join(self.temp, "doc.pdf"), (9999999999, 9999999999))
        self.assertEqual(self.serveur.lister_temp(self.temp)[0]["chemin"], "doc.pdf")   # le plus récent d'abord
        self.assertEqual(self.serveur.lister_temp(os.path.join(self.tmp, "absent")), [])

    def test_refus(self):
        for chemin in ("", "../secret.txt", "sous/../../secret.txt", os.path.join(self.tmp, "secret.txt"), "lien.txt",
                       "lien_dossier/secret.txt", "tube", "sous", ".cache.txt", ".cache/x.txt", "absent.txt", "a\0b",
                       "mauvais\ufffd.txt", "sous//rapport.md"):
            self.assertEqual(self.fichier(chemin)[0], 404, chemin)
        self.serveur.Lanceur.ce_poste = lambda self: False  # appareil non connecté
        self.assertEqual(self.demander("/api/temp")[0], 401)
        self.assertEqual(self.fichier("notes.txt")[0], 401)

    def test_rien_ne_tourne_sur_le_launcher(self):
        for chemin, type_, cadre, csp in (("a.png", "image/png", "DENY", False), ("notes.txt", "text/plain", "DENY", False),
                                          ("page.html", "text/html", "SAMEORIGIN", True), ("image.SVG", "image/svg+xml", "DENY", True),
                                          ("doc.pdf", "application/pdf", "SAMEORIGIN", False)):
            code, e, _ = self.fichier(chemin)
            self.assertEqual(code, 200, chemin)
            self.assertTrue(e["Content-Type"].startswith(type_), chemin)
            self.assertEqual(e["X-Frame-Options"], cadre, chemin)
            self.assertEqual(e["X-Content-Type-Options"], "nosniff", chemin)
            self.assertEqual("sandbox;" in (e["Content-Security-Policy"] or ""), csp, chemin)
            self.assertIsNone(e["Content-Disposition"], chemin)
        code, e, corps = self.fichier("bin é.dat")
        self.assertEqual((code, corps), (200, b"a\0b"))
        self.assertEqual(e["Content-Type"], "application/octet-stream")
        self.assertEqual(e["Content-Disposition"], "attachment; filename*=UTF-8''bin%20%C3%A9.dat")

    def test_contenu_et_apercu(self):
        self.assertEqual(self.fichier("sous/rapport.md")[2], b"# r")
        self.assertEqual(self.fichier("notes.txt", "&apercu=1")[2], "été".encode())
        with open(os.path.join(self.temp, "gros.log"), "wb") as f:
            f.write(b"x" * (self.serveur.APERCU_TEMP + 10))
        corps = self.fichier("gros.log", "&apercu=1")[2]
        self.assertLess(len(corps), self.serveur.APERCU_TEMP + 100)
        self.assertIn(b"suite non affich", corps)
        self.assertEqual(len(self.fichier("gros.log")[2]), self.serveur.APERCU_TEMP + 10)


class Andy(unittest.TestCase):
    def setUp(self):
        import threading
        from lanceur import serveur
        self.serveur = serveur
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.addCleanup(setattr, andy, "FICHIER", andy.FICHIER)     # ne pas toucher aux vraies pesées du Pi
        andy.FICHIER = os.path.join(self.tmp, "andy.json")
        self.addCleanup(setattr, serveur.Lanceur, "gardien", serveur.Lanceur.gardien)
        self.addCleanup(setattr, serveur.Lanceur, "infos", serveur.Lanceur.infos)
        self.addCleanup(setattr, serveur.Lanceur, "ce_poste", serveur.Lanceur.ce_poste)
        serveur.Lanceur.gardien = compte.Gardien(os.path.join(self.tmp, "c.json"), os.path.join(self.tmp, "s.json"))
        serveur.Lanceur.infos = {"port": 0, "dossier_outils": self.tmp}
        httpd = serveur.ThreadingHTTPServer(("127.0.0.1", 0), serveur.Lanceur)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        self.base = "http://127.0.0.1:%d" % httpd.server_address[1]

    def appeler(self, chemin, corps=None, entete=True):
        import urllib.error
        import urllib.request
        if isinstance(corps, (dict, list)):
            corps = json.dumps(corps).encode()
        req = urllib.request.Request(self.base + chemin, data=corps, headers={"X-Launcher": "1"} if entete else {})

        class SansRedirection(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *a):
                return None
        try:
            r = urllib.request.build_opener(SansRedirection).open(req, timeout=5)
        except urllib.error.HTTPError as e:
            r = e
        return r.status, r.read()

    def test_validation(self):
        auj = datetime.date(2026, 11, 5)
        for date, poids in (("2025-11-29", 5), ("2026-11-07", 5), ("2026-13-01", 5), ("hier", 5), (None, 5),
                            ("2026-11-05", 0.4), ("2026-11-05", 100.1), ("2026-11-05", True), ("2026-11-05", "13"),
                            ("2026-11-05", None), ("2026-11-05", float("nan"))):
            with self.assertRaises(ValueError, msg=(date, poids)):
                andy.ajouter(date, poids, auj)
        self.assertEqual(andy.lire(), [])
        andy.ajouter("2025-11-30", 0.5, auj)
        andy.ajouter("2026-11-06", 100, auj)               # un jour de marge pour le fuseau de l'appareil

    def test_ajouter_lire_supprimer(self):
        auj = datetime.date(2026, 11, 5)
        b = andy.ajouter("2026-11-05", 14.236, auj)
        a = andy.ajouter("2026-10-07", 13.6, auj)
        self.assertEqual(b["poids"], 14.24)
        self.assertEqual([p["date"] for p in andy.lire()], ["2026-10-07", "2026-11-05"])
        self.assertFalse(andy.supprimer("inconnu"))
        self.assertTrue(andy.supprimer(a["id"]))
        self.assertEqual([p["id"] for p in andy.lire()], [b["id"]])
        with open(andy.FICHIER, "w") as f:                  # fichier abîmé ou entrées bancales : ignorés, pas de plantage
            f.write('{"pesees": [{"id": "x", "date": "2026-01-01", "poids": 5}, {"id": 1}, 7, {"id": "y", "date": "d", "poids": "p"}]}')
        self.assertEqual([p["id"] for p in andy.lire()], ["x"])
        with open(andy.FICHIER, "w") as f:
            f.write("{pas du json")
        self.assertEqual(andy.lire(), [])
        self.assertEqual(andy.ajouter("2026-01-01", 5, auj)["poids"], 5)

    def test_api(self):
        code, corps = self.appeler("/api/andy/pesees")
        self.assertEqual((code, json.loads(corps)), (200, {"pesees": []}))
        self.assertEqual(self.appeler("/api/andy/pesees", {"date": "2026-01-01", "poids": 5}, entete=False)[0], 403)
        self.assertEqual(self.appeler("/api/andy/pesees", b"pas du json")[0], 400)
        self.assertEqual(self.appeler("/api/andy/pesees", [1, 2])[0], 400)
        self.assertEqual(self.appeler("/api/andy/pesees", {"date": "2026-01-01", "poids": 0})[0], 400)
        code, corps = self.appeler("/api/andy/pesees", {"date": "2026-01-01", "poids": 5.5})
        self.assertEqual(code, 200)
        pesee = json.loads(corps)["pesee"]
        self.assertEqual(json.loads(self.appeler("/api/andy/pesees")[1])["pesees"], [pesee])
        self.assertEqual(self.appeler("/api/andy/pesees/supprimer", {"id": "inconnu"})[0], 404)
        self.assertEqual(self.appeler("/api/andy/pesees/supprimer", {"id": pesee["id"]})[0], 200)
        self.assertEqual(json.loads(self.appeler("/api/andy/pesees")[1])["pesees"], [])
        self.assertEqual(self.appeler("/api/andy/pesees/autre", {})[0], 404)

    def test_acces_reserve_aux_comptes(self):
        self.assertEqual(self.appeler("/andy.html")[0], 200)
        self.assertEqual(self.appeler("/js/andy_donnees.json")[0], 200)
        self.serveur.Lanceur.ce_poste = lambda self: False      # appareil non connecté
        self.assertEqual(self.appeler("/andy.html")[0], 302)
        self.assertEqual(self.appeler("/js/andy_donnees.json")[0], 302)    # l'historique n'est pas public
        self.assertEqual(self.appeler("/js/andy.js")[0], 302)
        self.assertEqual(self.appeler("/api/andy/pesees")[0], 401)
        self.assertEqual(self.appeler("/api/andy/pesees", {"date": "2026-01-01", "poids": 5})[0], 401)
        self.assertEqual(self.appeler("/api/andy/pesees/supprimer", {"id": "x"})[0], 401)

    def test_donnees_livrees(self):
        with open(os.path.join(self.serveur.DOSSIER_WEB, "js", "andy_donnees.json"), encoding="utf-8") as f:
            evenements = json.load(f)
        self.assertGreater(len(evenements), 30)
        dates = [e["date"] for e in evenements]
        self.assertEqual(dates, sorted(dates))
        for e in evenements:
            datetime.date.fromisoformat(e["date"])
            self.assertTrue(e["titre"] and e["type"], e)


class Relais(unittest.TestCase):
    """Un outil ouvert à travers le launcher (/o/<id>/) : connexion, liens, redirections, WebSocket."""

    def setUp(self):
        import http.server
        import threading
        import types
        from lanceur import serveur
        self.serveur = serveur
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        test = self

        class Outil(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def repondre(self, corps, type_="text/plain", statut=200, entetes=()):
                if isinstance(corps, str):
                    corps = corps.encode()
                self.send_response(statut)
                self.send_header("Content-Type", type_)
                self.send_header("Content-Length", str(len(corps)))
                for k, v in entetes:
                    self.send_header(k, v)
                self.end_headers()
                self.wfile.write(corps)

            def do_GET(self):
                if self.headers.get("Upgrade", "").lower() == "websocket":
                    self.send_response(101)
                    self.send_header("Upgrade", "websocket")
                    self.send_header("Connection", "Upgrade")
                    self.end_headers()
                    self.wfile.flush()
                    test.vu_ws = (self.path, self.headers.get("Host"), self.headers.get("Origin"))
                    while True:
                        d = self.request.recv(1024)
                        if not d:
                            return
                        self.request.sendall(d.upper())
                if self.path == "/":
                    return self.repondre('<a href="/page">a</a><img src=\'/img.png\'><a href="//ext.example/x">b</a>'
                                         '<a href="relatif">c</a><form action="/envoyer">', "text/html; charset=utf-8")
                if self.path == "/page":
                    return self.repondre("page:" + self.headers["Host"])
                if self.path == "/redir":
                    return self.repondre("", statut=302, entetes=[("Location", "http://%s/page" % self.headers["Host"])])
                if self.path == "/cookie":
                    return self.repondre("ok", entetes=[("Set-Cookie", "a=1; Domain=exemple.fr; Path=/")])
                if self.path == "/flux":                   # sans Content-Length : fermé à la fin
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    self.wfile.write(b"data: 1\n\ndata: 2\n\n")
                    return
                self.repondre("introuvable", statut=404)

            def do_POST(self):
                corps = self.rfile.read(int(self.headers.get("Content-Length") or 0))
                vu = {k: self.headers.get(k) for k in ("Host", "Origin", "Referer", "Cookie", "Authorization")}
                vu["taille"] = len(corps)
                vu["somme"] = sum(corps) % 65536
                self.repondre(json.dumps(vu), "application/json")

        outil = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Outil)
        threading.Thread(target=outil.serve_forever, daemon=True).start()
        self.addCleanup(outil.server_close)
        self.addCleanup(outil.shutdown)
        self.port_outil = outil.server_address[1]
        morts = port_libre()
        for nom in ("gardien", "infos", "gestionnaire", "ce_poste"):
            self.addCleanup(setattr, serveur.Lanceur, nom, getattr(serveur.Lanceur, nom))
        self.mdp = "un-mot-de-passe-long"
        serveur.Lanceur.gardien = compte.Gardien(os.path.join(self.tmp, "c.json"), os.path.join(self.tmp, "s.json"))
        self.assertIsNone(serveur.Lanceur.gardien.ajouter("pilou", self.mdp))
        self.jeton, _ = serveur.Lanceur.gardien.verifier("pilou", self.mdp, "127.0.0.1")
        serveur.Lanceur.infos = {"port": 0, "dossier_outils": self.tmp}
        serveur.Lanceur.gestionnaire = types.SimpleNamespace(par_id={
            "faux": types.SimpleNamespace(port=self.port_outil), "mort": types.SimpleNamespace(port=morts)})
        httpd = serveur.ThreadingHTTPServer(("127.0.0.1", 0), serveur.Lanceur)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        self.port = httpd.server_address[1]
        self.hote = "127.0.0.1:%d" % self.port

    def demander(self, methode, chemin, corps=None, connecte=True, **entetes):
        import http.client
        h = {"Cookie": "launcher_session=%s" % self.jeton} if connecte else {}
        h.update({k.replace("_", "-"): v for k, v in entetes.items()})
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        c.request(methode, chemin, body=corps, headers=h)
        r = c.getresponse()
        donnees = r.read()
        c.close()
        return r.status, r, donnees

    def test_connexion_exigee(self):
        code, r, _ = self.demander("GET", "/o/faux/", connecte=False, Accept="text/html")
        self.assertEqual((code, r.getheader("Location")), (302, "/connexion.html"))
        self.assertEqual(self.demander("GET", "/o/faux/page", connecte=False)[0], 401)
        self.assertEqual(self.demander("POST", "/o/faux/echo", b"x", connecte=False)[0], 401)
        # le Referer d'un outil ne donne aucun droit
        self.assertEqual(self.demander("GET", "/page", connecte=False, Referer="http://%s/o/faux/" % self.hote)[0], 401)

    def test_page_et_liens(self):
        code, r, corps = self.demander("GET", "/o/faux/")
        self.assertEqual(code, 200)
        texte = corps.decode()
        self.assertIn('href="/o/faux/page"', texte)
        self.assertIn("src='/o/faux/img.png'", texte)
        self.assertIn('action="/o/faux/envoyer"', texte)
        self.assertIn('href="//ext.example/x"', texte)          # autre site : intact
        self.assertIn('href="relatif"', texte)
        self.assertEqual(int(r.getheader("Content-Length")), len(corps))
        self.assertEqual(r.getheader("Referrer-Policy"), "same-origin")
        self.assertIn("relais_outil=faux", r.getheader("Set-Cookie"))
        self.assertEqual(self.demander("GET", "/o/faux/page")[2], b"page:127.0.0.1:%d" % self.port_outil)

    def test_sans_barre_finale_et_inconnu(self):
        code, r, _ = self.demander("GET", "/o/faux")
        self.assertEqual((code, r.getheader("Location")), (302, "/o/faux/"))
        self.assertEqual(self.demander("GET", "/o/absent/")[0], 404)
        self.assertEqual(self.demander("GET", "/o/mort/")[0], 502)
        code, _, corps = self.demander("GET", "/o/faux/inconnu")
        self.assertEqual((code, corps), (404, b"introuvable"))   # le 404 de l'outil, tel quel

    def test_redirection_et_cookie(self):
        code, r, _ = self.demander("GET", "/o/faux/redir")
        self.assertEqual((code, r.getheader("Location")), (302, "/o/faux/page"))
        cookies = self.demander("GET", "/o/faux/cookie")[1].headers.get_all("Set-Cookie")
        self.assertEqual(cookies[0], "a=1; Path=/")
        self.assertTrue(cookies[1].startswith("relais_outil=faux;"))

    def test_referer_pour_les_liens_absolus(self):
        ref = "http://%s/o/faux/" % self.hote
        self.assertEqual(self.demander("GET", "/page", Referer=ref)[2], b"page:127.0.0.1:%d" % self.port_outil)
        self.assertEqual(self.demander("GET", "/page", Referer="http://%s/o/faux/sous/x" % self.hote)[0], 200)
        self.assertEqual(self.demander("GET", "/page", Referer="http://autre.example/o/faux/")[0], 404)   # autre site
        self.assertEqual(self.demander("GET", "/page")[0], 404)                                           # le launcher n'a pas /page
        self.assertEqual(self.demander("GET", "/api/inconnue")[0], 404)                                   # route du launcher

    def test_envoi_nettoye(self):
        gros = bytes(range(256)) * 1000                          # 256 000 octets
        code, _, corps = self.demander(
            "POST", "/o/faux/echo", gros, Origin="https://pi.tail1234.ts.net", Authorization="Basic eHg6eHg=",
            Referer="http://%s/o/faux/dossier/page?x=1" % self.hote, Cookie="launcher_session=%s; theme=sombre" % self.jeton)
        vu = json.loads(corps)
        self.assertEqual(code, 200)
        self.assertEqual((vu["taille"], vu["somme"]), (len(gros), sum(gros) % 65536))
        self.assertEqual(vu["Host"], "127.0.0.1:%d" % self.port_outil)
        self.assertEqual(vu["Origin"], "http://127.0.0.1:%d" % self.port_outil)
        self.assertEqual(vu["Referer"], "http://127.0.0.1:%d/dossier/page?x=1" % self.port_outil)
        self.assertEqual(vu["Cookie"], "theme=sombre")           # ni session du launcher, ni rien d'autre
        self.assertIsNone(vu["Authorization"])

    def test_flux_sans_longueur(self):
        code, r, corps = self.demander("GET", "/o/faux/flux")
        self.assertEqual((code, corps), (200, b"data: 1\n\ndata: 2\n\n"))
        self.assertEqual(r.getheader("Content-Type"), "text/event-stream")

    def test_launcher_inchange(self):
        # ni /o/ ni Referer : les routes du launcher répondent comme avant
        self.assertEqual(self.demander("GET", "/api/inconnue")[2], b'{"erreur": "route inconnue"}')
        self.assertEqual(self.demander("GET", "/connexion.html", connecte=False)[0], 200)
        self.assertEqual(self.demander("PUT", "/api/x")[0], 405)
        self.assertEqual(self.demander("POST", "/api/outils/faux/demarrer", b"{}")[0], 403)   # X-Launcher exigé

    def websocket(self, chemin, **entetes):
        import socket as _s
        s = _s.create_connection(("127.0.0.1", self.port), timeout=10)
        self.addCleanup(s.close)
        h = {"Host": self.hote, "Upgrade": "websocket", "Connection": "Upgrade", "Sec-WebSocket-Key": "abc",
             "Sec-WebSocket-Version": "13", "Origin": "https://pi.tail1234.ts.net"}
        h.update(entetes)
        s.sendall(("GET %s HTTP/1.1\r\n%s\r\n\r\n" % (chemin, "\r\n".join("%s: %s" % kv for kv in h.items()))).encode())
        return s

    def lire(self, s, jusqua):
        donnees = b""
        while jusqua not in donnees:
            morceau = s.recv(4096)
            if not morceau:
                break
            donnees += morceau
        return donnees

    def test_websocket(self):
        biscuit = "launcher_session=%s" % self.jeton
        s = self.websocket("/o/faux/ws", Cookie=biscuit)
        self.assertIn(b"101", self.lire(s, b"\r\n\r\n").split(b"\r\n")[0])
        s.sendall(b"bonjour")
        self.assertEqual(self.lire(s, b"BONJOUR"), b"BONJOUR")
        self.assertEqual(self.vu_ws, ("/ws", "127.0.0.1:%d" % self.port_outil, "http://127.0.0.1:%d" % self.port_outil))
        # chemin absolu /ws : l'outil est retrouvé par le cookie du relais
        s2 = self.websocket("/ws", Cookie=biscuit + "; relais_outil=faux")
        self.assertIn(b"101", self.lire(s2, b"\r\n\r\n").split(b"\r\n")[0])
        s2.sendall(b"ok")
        self.assertEqual(self.lire(s2, b"OK"), b"OK")

    def test_websocket_connexion_exigee(self):
        s = self.websocket("/o/faux/ws")
        self.assertIn(b"401", self.lire(s, b"\r\n").split(b"\r\n")[0])


class Claude(unittest.TestCase):
    def test_dossiers_et_liste(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp, True)
        for d in ("a/.git", "a/b/.git", "a/pas_git", "c", ".cache", "x y", "temp"):
            os.makedirs(os.path.join(tmp, d))
        lancees = []
        for nom, valeur in (("DOCUMENTS", tmp), ("LISTE", os.path.join(tmp, "liste")),
                            ("_lancer", lambda *c: lancees.append(c)), ("en_marche", lambda: {"claude"})):
            p = mock.patch.object(claude, nom, valeur)
            p.start()
            self.addCleanup(p.stop)
        self.assertEqual(claude.candidats(), ["a", "a/b", "c"])
        for chemin in ("../etc", "x y", "a/pas_git", ".cache"):
            with self.assertRaises(ValueError):
                claude.activer(chemin, True)
        claude.activer("a/b", True)
        claude.activer("c", True)
        claude.activer("a/b", False)
        self.assertEqual(claude.lire_liste(), ["c"])
        with self.assertRaises(ValueError):
            claude.redemarrer("a/b")                       # plus dans la liste
        self.assertEqual(lancees, [(claude.SERVEURS,), (claude.SERVEURS,)])
        lancees.clear()
        for fn in (claude.eteindre, claude.allumer):
            with self.assertRaises(ValueError):
                fn("a/b")
        claude.eteindre("c")                               # reste dans la liste
        claude.allumer("c")
        claude.eteindre()
        claude.allumer()
        self.assertEqual(claude.lire_liste(), ["c"])
        self.assertEqual(lancees, [(claude.SERVEURS,), ("systemctl", "--user", "stop", claude.SERVICE),
                                   ("systemctl", "--user", "start", claude.SERVICE)])

    def test_confiance(self):
        lancees = []
        with mock.patch.object(claude, "lire_liste", lambda: ["c"]), \
                mock.patch.object(claude, "_lancer", lambda *c: lancees.append(c)):
            with self.assertRaises(ValueError):
                claude.faire_confiance("autre")
            with mock.patch.object(claude, "confiance_attendue", lambda c: False):
                with self.assertRaises(ValueError):
                    claude.faire_confiance("c")
            with mock.patch.object(claude, "confiance_attendue", lambda c: True):
                claude.faire_confiance("c")
        self.assertEqual(lancees, [("tmux", "send-keys", "-t", "=c:", "y", "Enter")])


if __name__ == "__main__":
    unittest.main()
