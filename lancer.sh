#!/bin/sh
# Web Launcher (Raspberry Pi, Linux) : obtient l'exécutable (binaires.sh) puis le lance.
#   sh lancer.sh [--port 8090] [--dossier ~/Documents] [--sans-mdns] [--compte]
# Service systemd : ExecStart=/bin/sh .../web_launcher/lancer.sh (voir installer.sh).
cd "$(dirname "$0")" || exit 1
. ./binaires.sh
obtenir_binaires || exit 1
# exec : le launcher prend le PID du service. Après sa propre mise à jour, il reprend lui-même
# son nouvel exécutable (binaires.sh) et se relance sous le même PID : les outils restent ses enfants.
exec bin/launcher "$@"
