#!/bin/sh
# Met l'exécutable du launcher dans bin/ (à inclure par « . ./scripts/binaires.sh » depuis la racine).
# Avec cargo sur le poste : compilé ici, toujours à jour avec les sources.
# Sinon : téléchargé depuis GitHub (publication « binaires », compilée par la CI à chaque
# envoi sur main), une fois par version : bin/commit dit de quelle version il vient.
DEPOT_BINAIRES="https://github.com/pilou33620/web_launcher/releases/download/binaires"

# L'archive téléchargée doit avoir l'empreinte SHA-256 publiée avec elle : tronquée ou altérée en
# route, elle est refusée. (L'empreinte vient du même dépôt : elle ne protège pas d'un compte volé.)
empreinte_juste() {
  if command -v sha256sum >/dev/null 2>&1; then recu=$(sha256sum bin/.archive); else recu=$(shasum -a 256 bin/.archive); fi
  [ -n "$recu" ] && [ "${recu%% *}" = "$(cat bin/.empreinte)" ] && return 0
  echo "  [!] Empreinte de l'archive incorrecte : archive refusée."
  return 1
}

obtenir_binaires() {
  mkdir -p bin
  if command -v cargo >/dev/null 2>&1; then
    cargo build --release -p launcher --bins || return 1
    for f in target/release/*; do
      if [ -f "$f" ] && [ -x "$f" ]; then
        # copie puis renommage : un exécutable en cours d'usage ne se réécrit pas
        cp "$f" "bin/.neuf" && mv -f "bin/.neuf" "bin/$(basename "$f")"
      fi
    done
    return 0
  fi
  ici=$(git rev-parse HEAD 2>/dev/null)
  if [ -x bin/launcher ] && [ -n "$ici" ] && [ "$(cat bin/commit 2>/dev/null)" = "$ici" ]; then
    return 0
  fi
  case "$(uname -s)" in Linux) os=linux ;; Darwin) os=macos ;; *) os=inconnu ;; esac
  case "$(uname -m)" in x86_64|amd64) cpu=x86_64 ;; aarch64|arm64) cpu=aarch64 ;; *) cpu=inconnu ;; esac
  echo "  Téléchargement du launcher ($os-$cpu)..."
  archive="launcher-$os-$cpu.tar.gz"
  # délais bornés : sans réseau au démarrage du Pi, le launcher déjà présent part quand même
  if curl -fsSL --connect-timeout 15 --max-time 300 -o bin/.archive "$DEPOT_BINAIRES/$archive" \
     && curl -fsSL --connect-timeout 15 --max-time 60 -o bin/.empreinte "$DEPOT_BINAIRES/$archive.sha256" \
     && empreinte_juste && tar -xzf bin/.archive -C bin; then
    rm -f bin/.archive bin/.empreinte
    return 0
  fi
  rm -f bin/.archive bin/.empreinte
  if [ -x bin/launcher ]; then
    echo "  [!] Téléchargement impossible : on garde l'exécutable déjà présent."
    return 0
  fi
  echo "[X] Pas d'exécutable pour ce poste ($os-$cpu) et téléchargement impossible."
  echo "    Installer Rust (https://rustup.rs) puis relancer : il sera compilé ici."
  return 1
}
