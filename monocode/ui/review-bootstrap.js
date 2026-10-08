/* Only the isolated review document loads this. */
(function(){'use strict';const query=new URLSearchParams(location.search),skin=query.get('skin')==='existing'?'existing':'phoenix';document.documentElement.dataset.skin=skin;if(!localStorage.getItem('phoenix-theme'))localStorage.setItem('phoenix-theme',skin==='phoenix'?'dark':'light');})();
