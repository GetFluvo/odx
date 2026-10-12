"""Sphinx configuration for the odx documentation."""

project = "odx"
author = "Strategic Efficiency B.V."
copyright = f"2026, {author}"

extensions = [
    "myst_parser",
    "sphinx_copybutton",
]

html_theme = "shibuya"
html_title = "odx"
html_static_path = ["_static"]
html_logo = "_static/logo.svg"
html_favicon = "_static/favicon.png"
html_css_files = ["custom.css"]

html_theme_options = {
    "accent_color": "orange",
    "github_url": "https://github.com/getfluvo/odx",
}

myst_enable_extensions = [
    "colon_fence",
    "deflist",
]
myst_heading_anchors = 3

exclude_patterns = ["_build"]
