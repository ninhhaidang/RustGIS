# Layers pane

How a map's layers are picked and acted on in the Layers pane, and the words the pane, the commands and the docs share for it.

## Language

**Active layer**:
The one layer the commands that need a layer act on (Style and Data tabs, attribute table, Zoom To Layer). It is always part of the layer selection.
_Avoid_: Current layer, selected layer

**Layer selection**:
The layers picked together in the Layers pane for an action on all of them (remove, show or hide, move, group). It is kept with the open project's state, not saved in the project file.
_Avoid_: Multi-select, layer set

**Feature selection**:
The features picked in a layer, shown by its count badge and the Selection view. A bare "selection" always means this.
_Avoid_: Layer selection (a different thing: it picks layers, not features)
