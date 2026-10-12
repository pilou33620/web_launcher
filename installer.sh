#!/bin/sh
# Web Launcher : installation sur le Raspberry Pi (service systemd, démarre à l'allumage).
#   ./installer.sh            installe (ou met à jour) le service et le démarre
#   ./installer.sh --retirer  arrête et supprime le service (les outils ne sont pas touchés)
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
SERVICE=web_launcher
UNIT=/etc/systemd/system/$SERVICE.service
UTILISATEUR="$(id -un)"

if [ "$1" = "--retirer" ]; then
  sudo systemctl disable --now "$SERVICE" 2>/dev/null || true
  sudo rm -f "$UNIT"
  sudo systemctl daemon-reload
  echo "Service $SERVICE retiré."
  exit 0
fi

oui() { printf "%s [o/N] " "$1"; read -r r; case "$r" in o|O|oui|y|Y) return 0;; *) return 1;; esac; }

echo "== Paquets nécessaires"
MANQUE=""
command -v python3 >/dev/null || MANQUE="$MANQUE python3"
command -v git >/dev/null || MANQUE="$MANQUE git"
command -v avahi-publish >/dev/null || MANQUE="$MANQUE avahi-utils"
python3 -c 'import ensurepip' 2>/dev/null || MANQUE="$MANQUE python3-venv"
if [ -n "$MANQUE" ]; then
  echo "   installation de :$MANQUE"
  sudo apt-get update -qq && sudo apt-get install -y $MANQUE
else
  echo "   tout est déjà là."
fi
PY="$(command -v python3)"
"$PY" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)' || { echo "Python 3.8 ou plus récent est nécessaire."; exit 1; }

# Les web tools lancés jusqu'ici par leur propre service : c'est le launcher qui s'en charge désormais.
for ancien in gamelles web_conquete; do
  if systemctl is-enabled "$ancien" >/dev/null 2>&1; then
    echo
    echo "Le service « $ancien » démarre déjà un web tool à l'allumage."
    if oui "Le désactiver pour laisser le launcher le gérer (recommandé) ?"; then
      sudo systemctl disable --now "$ancien"
    else
      echo "   gardé : le launcher le reprendra quand même s'il tourne (bouton Arrêter compris)."
    fi
  fi
done

echo
echo "== Compte (identifiant et mot de passe de la page web)"
if [ -f "$DIR/compte.local.json" ]; then
  echo "   déjà créé (le changer : python3 launcher.py --compte)."
else
  until "$PY" "$DIR/launcher.py" --compte; do echo "   Recommencer."; done
fi

echo
echo "== Service $SERVICE"
sudo tee "$UNIT" > /dev/null <<UNITEOF
[Unit]
Description=Web Launcher (web tools du Raspberry Pi)
After=network-online.target avahi-daemon.service
Wants=network-online.target

[Service]
User=$UTILISATEUR
WorkingDirectory=$DIR
ExecStart=$PY $DIR/launcher.py
Restart=always
RestartSec=3
Environment=PYTHONUNBUFFERED=1
# Port 80 sans être root : « http://launcher.local » sans numéro de port.
AmbientCapabilities=CAP_NET_BIND_SERVICE
# Arrêter ou redémarrer le launcher n'arrête pas les web tools : il les retrouve en repartant.
KillMode=process

[Install]
WantedBy=multi-user.target
UNITEOF
sudo systemctl daemon-reload
sudo systemctl enable "$SERVICE" >/dev/null 2>&1
sudo systemctl restart "$SERVICE"

# Dépôts privés (web_menu, projets WEB·SUITE) : git doit pouvoir les lire sans mot de passe.
if command -v gh >/dev/null 2>&1; then
  if ! gh auth status >/dev/null 2>&1; then
    echo
    if oui "Connecter ce Raspberry Pi à GitHub (dépôts privés) maintenant ?"; then
      gh auth login --hostname github.com --git-protocol https --web && gh auth setup-git
    fi
  fi
else
  echo
  echo "Dépôts privés (web_menu...) : voir README, « Dépôts privés » (sudo apt install gh, puis gh auth login)."
fi

IP=""
for i in $(seq 1 10); do
  IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
  [ -n "$IP" ] && break
  sleep 1
done
sleep 2
if ! systemctl is-active --quiet "$SERVICE"; then
  echo "Le service ne démarre pas : journalctl -u $SERVICE -n 50"
  exit 1
fi
echo
echo "Web Launcher tourne et démarrera à chaque allumage. Depuis un appareil du réseau local :"
echo "    http://launcher.local"
[ -n "$IP" ] && echo "    http://$IP"
echo "    http://$(hostname).local"
echo
echo "État  : systemctl status $SERVICE"
echo "Suivi : journalctl -u $SERVICE -f"
