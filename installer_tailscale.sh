#!/bin/sh
# Tailscale sur le Raspberry Pi : atteindre le launcher et les web tools depuis n'importe où.
#   ./installer_tailscale.sh   installe Tailscale (dépôt apt officiel), puis affiche le lien de connexion
# Ensuite : http://100.x.y.z (launcher, port 80) et http://100.x.y.z:<port> (outils).
# Utiliser l'IP 100.x, pas le nom *.ts.net : les outils refusent un nom d'hôte qu'ils ne connaissent pas.
set -e

if ! command -v tailscale >/dev/null; then
  . /etc/os-release
  echo "== Installation de Tailscale ($ID $VERSION_CODENAME)"
  sudo mkdir -p /usr/share/keyrings
  curl -fsSL "https://pkgs.tailscale.com/stable/$ID/$VERSION_CODENAME.noarmor.gpg" \
    | sudo tee /usr/share/keyrings/tailscale-archive-keyring.gpg >/dev/null
  curl -fsSL "https://pkgs.tailscale.com/stable/$ID/$VERSION_CODENAME.tailscale-keyring.list" \
    | sudo tee /etc/apt/sources.list.d/tailscale.list >/dev/null
  sudo apt-get update -qq
  sudo apt-get install -y tailscale
fi
sudo systemctl enable --now tailscaled

echo "== Connexion : ouvre le lien affiché ci-dessous et valide avec ton compte"
sudo tailscale up

echo
echo "Tailscale est connecté. Adresse du Pi : http://$(tailscale ip -4 | head -n1)"
