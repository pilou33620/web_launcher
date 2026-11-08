"""Test de nettoyer_branches sur un dépôt local (aucun accès réseau).

    python3 -m unittest tests.test_nettoyer_branches
"""
import contextlib
import io
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import nettoyer_branches  # noqa: E402


def g(depot, *args):
    subprocess.run(["git", "-C", depot, "-c", "user.name=t", "-c", "user.email=t@t", *args],
                   check=True, capture_output=True)


def commit(depot, fichier, texte):
    with open(os.path.join(depot, fichier), "w") as f:
        f.write(texte)
    g(depot, "add", fichier)
    g(depot, "commit", "-m", fichier + texte)


class Test(unittest.TestCase):
    def test_supprime_seulement_le_fusionne(self):
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp)
        distant, d = os.path.join(tmp, "distant.git"), os.path.join(tmp, "racine", "depot")
        subprocess.run(["git", "init", "-q", "--bare", "-b", "main", distant], check=True)
        subprocess.run(["git", "clone", "-q", distant, d], check=True, capture_output=True)
        g(d, "checkout", "-q", "-b", "main")
        commit(d, "a", "1")
        for nom, fichier in (("classique", "b"), ("squash", "c"), ("encours", "e"), ("conflit", "a")):
            g(d, "checkout", "-q", "-b", nom, "main")
            commit(d, fichier, nom)
        g(d, "checkout", "-q", "main")
        g(d, "merge", "-q", "--no-ff", "classique", "-m", "fusion")
        g(d, "merge", "-q", "--squash", "squash")
        g(d, "commit", "-m", "squash")
        commit(d, "a", "2")  # main avance sur a : « conflit » n'est plus fusionnable proprement
        g(d, "push", "-q", "origin", "main", "classique", "squash", "encours", "conflit")

        sortie = io.StringIO()
        with mock.patch("builtins.input", return_value="o"), contextlib.redirect_stdout(sortie):
            nettoyer_branches.main(["x", os.path.join(tmp, "racine")])

        restent = lambda p: sorted(nettoyer_branches.branches(d, p))  # noqa: E731
        self.assertEqual(restent("refs/heads/"), ["conflit", "encours"])
        g(d, "fetch", "-q", "--prune", "origin")
        self.assertEqual(restent("refs/remotes/origin/"), ["conflit", "encours"])
        self.assertNotIn("ÉCHEC", sortie.getvalue())


if __name__ == "__main__":
    unittest.main()
