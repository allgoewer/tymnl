#import "tymnl.typ"

#let (render, modules, mod, inputs) = tymnl.init(colors: tymnl.colors.light)
#show: render.with(info: "Setup")

#let mac = sys.inputs.at("mac", default: "??:??:??:??:??:??")

#set align(center + horizon)

#text(size: 2em, weight: "bold")[Welcome to tyMNL!]

#v(0.5em)
This device isn't set up yet. Add it to your `tymnl.yml`:
#v(0.3em)

#set align(left)
#raw(
  block: true,
  lang: "yaml",
  "device:\n  - name: My Device\n    mac_address: " + mac,
)
