// Make the book title in the top bar link back to the devsync homepage.
(function () {
  var title = document.querySelector(".menu-title");
  if (!title || title.closest("a")) return;
  var link = document.createElement("a");
  link.href = (window.path_to_root || "") + "../";
  link.title = "devsync home";
  link.style.color = "inherit";
  link.style.textDecoration = "none";
  title.parentNode.insertBefore(link, title);
  link.appendChild(title);
})();
