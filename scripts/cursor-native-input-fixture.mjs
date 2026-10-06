// A real GTK text target, confined to the headless acceptance display.
import Gtk from 'gi://Gtk?version=4.0';
import GLib from 'gi://GLib';
import Graphene from 'gi://Graphene';
const root=ARGV[0]||GLib.getenv('XDG_RUNTIME_DIR')?.replace(/\/runtime$/,'');
const standalone=root?.startsWith('/tmp/phoenix-native-cursor-')&&GLib.getenv('WAYLAND_DISPLAY')==='phoenix-test';
const scope=GLib.getenv('PHOENIX_DESKTOP_SCOPE'),home=GLib.getenv('PHOENIX_HOME');
const owned=home?.startsWith('/tmp/')&&/^desktop-[a-f0-9]{24}$/.test(scope||'')&&
  root===`${home}/desktops/${scope}`&&GLib.getenv('WAYLAND_DISPLAY')==='phoenix-agent';
if((!standalone&&!owned)||GLib.getenv('XDG_RUNTIME_DIR')!==`${root}/runtime`)throw Error('Private input fixture scope missing');
const app=new Gtk.Application({application_id:'dev.phoenix.CursorAcceptance'});
app.connect('activate',()=>{
  const window=new Gtk.ApplicationWindow({application:app,title:'Phoenix native input acceptance',default_width:520,default_height:450});
  const box=new Gtk.Box({orientation:Gtk.Orientation.VERTICAL,spacing:16,margin_top:24,margin_bottom:24,margin_start:24,margin_end:24});
  box.append(new Gtk.Label({label:'Native cursor acceptance — disposable text target'}));
  const entry=new Gtk.Entry();
  const button=new Gtk.Button({label:'Native click target'});
  const slider=Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL,0,100,1);
  const document=new Gtk.ScrolledWindow({height_request:110,vexpand:true});
  const documentRows=new Gtk.Box({orientation:Gtk.Orientation.VERTICAL,spacing:8});
  for(let row=1;row<=50;row++)documentRows.append(new Gtk.Label({label:`Native document — line ${row}`,xalign:0}));
  document.set_child(documentRows);
  const documentState=()=>{
    const [valid,rect]=document.compute_bounds(window),adjustment=document.get_vadjustment();
    return valid?{x:rect.origin.x,y:rect.origin.y,width:rect.size.width,height:rect.size.height,
      offset:adjustment.get_value(),upper:adjustment.get_upper(),page:adjustment.get_page_size()}:null;
  };
  slider.set_value(20);
  const sliderValues=[20];
  slider.connect('value-changed',()=>sliderValues.push(slider.get_value()));
  const sliderState=()=>{
    const [valid,rect]=slider.compute_bounds(window);
    const range=slider.get_range_rect(),[start,end]=slider.get_slider_range();
    const [thumbValid,thumb]=slider.compute_point(window,new Graphene.Point({x:(start+end)/2,y:range.y+range.height/2}));
    // GtkScale's trough already describes the value axis. Subtracting the
    // painted thumb width again shortens the requested drag incorrectly.
    const targetX=range.x+range.width*0.8;
    const [targetValid,target]=slider.compute_point(window,new Graphene.Point({x:targetX,y:range.y+range.height/2}));
    return valid?{value:slider.get_value(),values:[...sliderValues],x:rect.origin.x,y:rect.origin.y,width:rect.size.width,
      range:{x:range.x,y:range.y,width:range.width,height:range.height},start,end,
      thumbX:(start+end)/2,trackY:range.y+range.height/2,
      windowThumb:thumbValid?{x:thumb.x,y:thumb.y}:null,windowTarget:targetValid?{x:target.x,y:target.y}:null}:null;
  };
  let clicks=0,hoverEntries=0;
  const hover=new Gtk.EventControllerMotion();
  hover.connect('enter',()=>{hoverEntries++;});
  button.add_controller(hover);
  const buttonBounds=()=>{
    const [valid,rect]=button.compute_bounds(window);
    return valid?{x:rect.origin.x,y:rect.origin.y,width:rect.size.width,height:rect.size.height}:null;
  };
  let other=null,otherEntry=null;
  const keys=[],keycodes=[];
  const controller=new Gtk.EventControllerKey({propagation_phase:Gtk.PropagationPhase.CAPTURE});
  controller.connect('key-pressed',(_controller,key,keycode)=>{keys.push(key);keycodes.push(keycode);return false;});
  window.add_controller(controller);
  const scrollEvents=[];
  const scrollController=new Gtk.EventControllerScroll({flags:Gtk.EventControllerScrollFlags.BOTH_AXES|Gtk.EventControllerScrollFlags.DISCRETE,propagation_phase:Gtk.PropagationPhase.CAPTURE});
  scrollController.connect('scroll',(_controller,dx,dy)=>{scrollEvents.push({dx,dy});return false;});
  window.add_controller(scrollController);
  const save=()=>GLib.file_set_contents(`${root}/input-state.json`,JSON.stringify({text:entry.get_text(),active:window.is_active,focus:window.get_focus()?.constructor.name,keys,keycodes,clicks,hoverEntries,scrollEvents,document:documentState(),button:buttonBounds(),slider:sliderState(),otherText:otherEntry?.get_text(),otherActive:other?.is_active}));
  button.connect('clicked',()=>{clicks++;entry.grab_focus();save();});
  entry.connect('changed',save);box.append(entry);box.append(button);box.append(slider);box.append(document);window.set_child(box);window.present();entry.grab_focus();save();
  GLib.timeout_add(GLib.PRIORITY_DEFAULT,100,()=>{
    if(!other&&GLib.file_test(`${root}/focus-other`,GLib.FileTest.EXISTS)) {
      other=new Gtk.ApplicationWindow({application:app,title:'Unrelated native input target',default_width:520,default_height:180});
      otherEntry=new Gtk.Entry({margin_top:40,margin_bottom:40,margin_start:24,margin_end:24});
      other.set_child(otherEntry);other.present();otherEntry.grab_focus();
    }
    save();return GLib.SOURCE_CONTINUE;
  });
});
app.run([]);
