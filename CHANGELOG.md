# Changelog

Une ligne par version publiée ([semver](https://semver.org/lang/fr/)) : date et résumé. Avant
0.8.0, l'historique est dans `ROADMAP.md` et `git log`.

- **0.8.1** (2026-10-15) — Téléphone : le clavier s'ouvre au premier tap dans un champ et revient
  quand on tape à nouveau après l'avoir rangé (il fallait un appui long) ; la fenêtre de
  récupération des documents non enregistrés tient dans l'écran (titre replié, nom de fichier
  raccourci) ; la reconnaissance de texte trouve ses modèles dans un APK construit sous Windows.
- **0.8.0** (2026-10-15) — Téléphone : transitions entre écrans, assistant IA au choix (Claude,
  ChatGPT, Gemini ou un service compatible), cible Android 16 (marges système lues par l'app,
  bibliothèque alignée sur 16 Ko) ; accessibilité : focus visible, pages et titres lus par les
  lecteurs d'écran, cibles de 48, contrastes des erreurs et avertissements ; notifications
  empilées avec Annuler ; robustesse et sécurité (biffures et JavaScript bornés, canal de
  contrôle durci, récupération multi-fenêtre) ; performances (autosave en arrière-plan, recherche
  bornée, rendu qui réveille l'interface).
