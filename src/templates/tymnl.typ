#let colors = (
  light: (fg: black, bg: white, mid: luma(85), light: luma(170)),
  dark: (fg: white, bg: black, mid: luma(170), light: luma(85)),
)

#let px_to_dimensions(x, y, dpi) = (
  width: x / dpi * 2.54cm,
  height: y / dpi * 2.54cm,
)

#let init(colors: colors.light) = {
  let in-tymnl = "tymnl-internal" in sys.inputs

  let inputs = if in-tymnl {
    let inputs = json(bytes(sys.inputs.tymnl-internal))
    (
      screen-name: inputs.screen-name,
      battery: inputs.battery-percentage,
      id: inputs.id,
      current-time: inputs.next-update-at,
    )
  } else {
    (screen-name: "", battery: "0", id: "Unknown", current-time: "00:00")
  }


  let render(colors: colors, info: auto, id: auto, batt: auto, inputs: inputs, content) = {
    set text(
      fill: colors.fg,
      font: "Space Grotesk",
      weight: "medium",
      stroke: 0.05pt + colors.fg,
      hyphenate: true,
    )
    set page(
      ..px_to_dimensions(800, 480, 128),
      margin: (top: 1pt, bottom: 12pt, x: 0pt),
      fill: colors.bg,
      footer-descent: 10%,
      footer: {
        set align(left + horizon)
        set text(size: 10pt, fill: colors.bg, weight: "bold")
        box(fill: colors.fg, width: 100%, height: 100%, inset: .2em)[
          #grid(
            columns: (3fr, 2fr, 2.5fr, 1fr),
            align: (left, center, center, right),
            if info == auto [#inputs.screen-name] else { info },
            if id == auto [#inputs.id] else { id },
            [Last update: #inputs.current-time],
            if batt == auto [Batt: #inputs.battery %] else [Batt: #batt %],
          )
        ]
      },
    )
    set table(
      inset: .4em,
      stroke: none,
      fill: (_, y) => {
        if y == 0 { colors.fg }
      },
    )
    show table.cell.where(y: 0): set text(fill: colors.bg, weight: "extrabold")
    show heading: set block(below: .75em)

    set align(center + horizon)

    content
  }

  let mod(caption, height: auto, colors: colors, content) = {
    box(
      width: 100%,
      height: height,
      outset: -3pt,
      inset: (x: 8pt, top: 8pt),
      stroke: (paint: colors.fg, thickness: 1pt, dash: "dotted"),
      radius: 5pt,
    )[
      #content
      #v(1fr)
      #align(
        left,
        box(
          width: 100%,
          inset: (top: 3pt, bottom: 7pt, x: 2pt),
          stroke: (top: (paint: colors.fg, thickness: 1pt, dash: "dotted")),
          text(size: 11pt, weight: "bold", caption),
        ),
      )
    ]
  }

  let modules(top-height: auto, ..mods) = {
    let mods = mods.pos()
    top-height = if top-height == auto { 1fr } else { top-height }

    if mods.len() == 1 { mods.at(0) } else if mods.len() == 2 [
      #block(height: top-height, below: 0pt, mods.at(0))
      #block(height: 1fr, mods.at(1))
    ] else if mods.len() == 3 [
      #block(height: top-height, below: 0pt, mods.at(0))
      #block(height: 1fr, grid(
        columns: 2,
        mods.at(1), mods.at(2),
      ))
    ] else if mods.len() == 4 [
      #block(height: top-height, grid(
        columns: 2,
        mods.at(0), mods.at(1),
      ))
      #block(height: 1fr, grid(
        columns: 2,
        mods.at(2), mods.at(3),
      ))
    ]
  }

  (render: render, modules: modules, mod: mod, inputs: inputs, colors: colors)
}
