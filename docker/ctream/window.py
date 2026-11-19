import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk


window = Gtk.Window(title="Ctream Wayland fixture")
window.set_default_size(500, 320)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=20)
box.set_border_width(30)
window.add(box)
entry = Gtk.Entry()
entry.set_placeholder_text("Fixture text")
box.pack_start(entry, False, False, 0)
button = Gtk.Button(label="Fixture button")
box.pack_start(button, False, False, 0)
button.connect("clicked", lambda _: button.set_label("Clicked"))
window.connect("destroy", Gtk.main_quit)
window.show_all()
Gtk.main()
