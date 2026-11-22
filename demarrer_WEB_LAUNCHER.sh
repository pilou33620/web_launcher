#!/bin/sh
# Web Launcher (Raspberry Pi, Linux) : obtient l'exécutable (scripts/binaires.sh) puis le lance.
#   sh demarrer_WEB_LAUNCHER.sh [--port 8090] [--dossier ~/Documents] [--sans-mdns] [--compte]
# Service systemd : ExecStart=/bin/sh .../web_launcher/demarrer_WEB_LAUNCHER.sh (voir installer.sh).
cd "$(dirname "$0")" || exit 1
. ./scripts/binaires.sh
if ! obtenir_binaires; then
  # pause seulement devant un vrai terminal (pas sous systemd)
  if [ -t 0 ] && [ "$TERM" != dumb ]; then
    echo
    echo "[X] Le démarrage a échoué. La raison est écrite au-dessus."
    printf "Appuyez sur Entrée pour fermer..."; read -r _
  fi
  exit 1
fi
# exec : le launcher prend le PID du service. Après sa propre mise à jour, il reprend lui-même
# son nouvel exécutable (scripts/binaires.sh) et se relance sous le même PID (pas de boucle sur
# le code 75 ici) : les outils restent ses enfants.
exec bin/launcher "$@"
