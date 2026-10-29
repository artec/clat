function installWorkspaceReview() {
  developmentGit = installDevelopmentGit();
  document.getElementById('review-open').addEventListener('click', developmentGit.open);
}
