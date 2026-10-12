"""Tests du launcher avec un faux web tool (dépôt git local, aucun accès réseau).

    python3 -m unittest discover tests
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from lanceur import config, reseau  # noqa: E402
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
        self.assertEqual(conf["dossier_outils"], os.path.expanduser("~/x"))

    def test_catalogue_livre(self):
        conf = config.charger(config.CATALOGUE, os.devnull + ".absent")
        ports = [o["port"] for o in conf["outils"]]
        self.assertEqual(len(ports), len(set(ports)))
        for o in conf["outils"]:
            self.assertIn("{port}", o["commande"])


if __name__ == "__main__":
    unittest.main()
