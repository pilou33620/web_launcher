"use strict";

const formulaire = document.getElementById("formulaire");
const erreur = document.getElementById("erreur");
const valider = document.getElementById("valider");

formulaire.addEventListener("submit", async (ev) => {
  ev.preventDefault();
  erreur.textContent = "";
  valider.disabled = true;
  valider.textContent = "Vérification…";
  try {
    const r = await fetch("/api/connexion", {
      method: "POST",
      headers: { "X-Launcher": "1", "Content-Type": "application/json" },
      body: JSON.stringify({
        login: document.getElementById("login").value,
        mot_de_passe: document.getElementById("mdp").value,
      }),
    });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.erreur || "Erreur " + r.status);
    location.replace("/");
  } catch (e) {
    erreur.textContent = e.message;
    document.getElementById("mdp").select();
  } finally {
    valider.disabled = false;
    valider.textContent = "Se connecter";
  }
});
