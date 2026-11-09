// Applies the saved theme before the first paint (loaded without defer, no inline code).
try{var t=localStorage.getItem('dman-theme');if(t==='light'||t==='dark')document.documentElement.setAttribute('data-theme',t)}catch(e){}
