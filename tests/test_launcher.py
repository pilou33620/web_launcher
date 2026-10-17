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

from lanceur import compte, config, reseau  # noqa: E402
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


if __name__ == "__main__":
    unittest.main()
