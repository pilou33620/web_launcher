#!/usr/bin/env python3
"""Web Launcher : allume, éteint, installe et met à jour les web tools du Raspberry Pi.

    python3 launcher.py                    # port 80 (service systemd), sinon 8090
    python3 launcher.py --port 8090
    python3 launcher.py --dossier ~/Documents

Bibliothèque standard uniquement. Voir README.md.
"""
import argparse
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from lanceur import andy, compte, config, reseau, serveur  # noqa: E402
from lanceur.gestionnaire import Gestionnaire  # noqa: E402


def main():
    # scrypt (32 Mo) dans des threads différents : sans cette limite, la glibc garde un tas par
    # thread et la mémoire ne redescend jamais. Lue au démarrage du processus : on se relance.
    if os.environ.get("MALLOC_ARENA_MAX") != "2":
        os.environ["MALLOC_ARENA_MAX"] = "2"
        os.execv(sys.executable, [sys.executable, os.path.abspath(__file__)] + sys.argv[1:])
    ap = argparse.ArgumentParser(description="Web Launcher : les web tools du Raspberry Pi.")
    ap.add_argument("--port", type=int, help="port d'écoute (défaut : outils.json, 80)")
    ap.add_argument("--dossier", help="dossier des web tools (défaut : ~/Documents)")
    ap.add_argument("--sans-mdns", action="store_true", help="ne pas annoncer launcher.local")
    ap.add_argument("--compte", action="store_true",
                    help="ajouter un compte ou changer un mot de passe, puis quitter")
    args = ap.parse_args()
    if args.compte:
        return 0 if compte.creer_compte() else 1

    conf = config.charger()
    if args.dossier:
        conf["dossier_outils"] = os.path.abspath(os.path.expanduser(args.dossier))
    if args.port:
        conf["port"], conf["port_secours"] = args.port, None

    print("Web Launcher", flush=True)
    print("  Outils dans %s" % conf["dossier_outils"], flush=True)
    httpd = serveur.ouvrir(int(conf["port"]), conf.get("port_secours"))
    port = httpd.server_address[1]

    andy.historique()      # reprend l'historique d'Andy de l'ancien emplacement, s'il le faut
    gestionnaire = Gestionnaire(conf, ecrire=lambda t: print(t, flush=True))
    gestionnaire.reprendre()

    alias = reseau.AliasMdns(None if args.sans_mdns else conf.get("nom_mdns"),
                             journal=lambda t: print(t, flush=True))
    alias.demarrer()

    serveur.Lanceur.gestionnaire = gestionnaire
    serveur.Lanceur.gardien = compte.Gardien()
    if not serveur.Lanceur.gardien.actif:
        print("  [!] Aucun mot de passe : seul ce poste (127.0.0.1) a accès, les autres appareils sont refusés. "
              "Créer le compte : python3 launcher.py --compte", flush=True)
    serveur.Lanceur.alias = alias
    serveur.Lanceur.relancer = staticmethod(lambda: (alias.arreter(), serveur.relancer_processus()))
    serveur.Lanceur.infos = {"port": port, "nom_mdns": conf.get("nom_mdns"),
                             "port_public_relais": conf.get("port_public_relais", 8443),
                             "nom_machine": reseau.nom_machine(),
                             "dossier_outils": conf["dossier_outils"], "demarre": int(time.time())}
    threading.Thread(target=gestionnaire.surveiller, daemon=True).start()
    # les outils depuis Internet : adresse à part (Caddy -> 127.0.0.1:port_relais), voir README
    relais = serveur.ouvrir_relais(int(conf.get("port_relais", 18089)), ecrire=lambda t: print(t, flush=True))
    if relais:
        threading.Thread(target=relais.serve_forever, daemon=True).start()

    suffixe = "" if port == 80 else ":%d" % port
    print("  Ouvrir dans un navigateur :", flush=True)
    ip = reseau.ip_locale()
    if ip:
        print("    http://%s%s/" % (ip, suffixe))
    ts = reseau.ip_tailscale()
    if ts:
        print("    http://%s%s/   (Tailscale)" % (ts, suffixe))
    if conf.get("nom_mdns") and not args.sans_mdns:
        print("    http://%s.local%s/" % (conf["nom_mdns"], suffixe))
    print("    http://%s.local%s/" % (reseau.nom_machine(), suffixe), flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nArrêt du launcher (les outils lancés continuent de tourner).")
    finally:
        alias.arreter()
        httpd.server_close()
        if relais:
            relais.server_close()


if __name__ == "__main__":
    sys.exit(main())
