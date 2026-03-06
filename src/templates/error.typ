#import "tymnl.typ"

#let (render, modules, mod, inputs) = tymnl.init(colors: tymnl.colors.light)
#show: render.with(info: "Error")

#set text(size: 9pt)
= #sys.inputs.at("error-title", default: "Error")

#raw(block: true, sys.inputs.at("error", default: ""))
